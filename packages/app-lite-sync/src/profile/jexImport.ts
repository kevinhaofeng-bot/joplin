import { lstat } from 'node:fs/promises';
import { extname, isAbsolute } from 'node:path';
import { IMPORT_ERROR_MESSAGES, ProtocolError } from '../protocol';

export const MAX_JEX_BYTES = 8 * 1024 * 1024 * 1024;

export type JexImportSummary = { notes: number; folders: number; tags: number; resources: number };
export type JexImportStatus =
	| { state: 'idle' | 'running' }
	| { state: 'succeeded'; summary: JexImportSummary }
	| { state: 'failed'; code: 'IMPORT_INVALID' | 'IMPORT_BUSY' | 'IMPORT_FAILED' };

export type JexImportAdapter = (path: string)=> Promise<JexImportSummary>;

export function importError(code: 'IMPORT_INVALID' | 'IMPORT_BUSY' | 'IMPORT_FAILED'): ProtocolError {
	return new ProtocolError(code, IMPORT_ERROR_MESSAGES[code]);
}

export async function validateJexPath(value: unknown): Promise<string> {
	if (typeof value !== 'string' || !isAbsolute(value) || value.includes('\0') || extname(value).toLowerCase() !== '.jex') throw importError('IMPORT_INVALID');
	try {
		const stat = await lstat(value);
		if (!stat.isFile() || stat.isSymbolicLink() || stat.size > MAX_JEX_BYTES) throw importError('IMPORT_INVALID');
		return value;
	} catch (error) {
		if (error instanceof ProtocolError) throw error;
		throw importError('IMPORT_INVALID');
	}
}

export class JexImportService {
	private status: JexImportStatus = { state: 'idle' };
	private active: { settled: Promise<void> } | undefined;

	public constructor(private readonly adapter: JexImportAdapter) {}

	public getStatus(): JexImportStatus {
		if (this.status.state === 'succeeded') return { state: 'succeeded', summary: { ...this.status.summary } };
		return this.status;
	}

	public async start(path: unknown): Promise<JexImportStatus> {
		if (this.active) throw importError('IMPORT_BUSY');
		const validated = await validateJexPath(path);
		if (this.active) throw importError('IMPORT_BUSY');
		this.status = { state: 'running' };
		const task = Promise.resolve().then(() => this.adapter(validated)).then(summary => {
			this.status = { state: 'succeeded', summary: { ...summary } };
		}, () => {
			this.status = { state: 'failed', code: 'IMPORT_FAILED' };
		});
		const settled: Promise<void> = task.then((): void => undefined, (): void => undefined);
		this.active = { settled };
		void task.catch((): void => undefined);
		void settled.then(() => {
			if (this.active?.settled === settled) this.active = undefined;
		});
		return { state: 'running' };
	}

	public async waitForIdle(): Promise<void> {
		await this.active?.settled;
	}
}
