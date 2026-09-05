export type SyncConfig = {
	configured: boolean;
	url?: string;
	username?: string;
};

export type SyncConfigInput = {
	url: string;
	username: string;
	password: string;
};

export type SyncSummary = {
	completedAt: number;
	created: number;
	updated: number;
	deleted: number;
	fetched: number;
};

export type SyncAdapter = {
	readConfig: ()=> Promise<SyncConfig>;
	configure: (input: SyncConfigInput)=> Promise<void>;
	syncNow: ()=> Promise<SyncSummary>;
};

const stableCodes = new Set(['SYNC_NOT_CONFIGURED', 'SYNC_AUTH_FAILED', 'SYNC_NETWORK', 'SYNC_BUSY', 'SYNC_FAILED']);

export function syncError(code: string): Error & { code: string } {
	const safeCode = stableCodes.has(code) ? code : 'SYNC_FAILED';
	return Object.assign(new Error(safeCode), { code: safeCode });
}

export function normalizeSyncUrl(value: string): string {
	if (typeof value !== 'string' || value.length === 0 || value.length > 4096 || value.includes('\0')) throw syncError('SYNC_NETWORK');
	let parsed: URL;
	try { parsed = new URL(value); } catch { throw syncError('SYNC_NETWORK'); }
	const local = ['localhost', '127.0.0.1', '[::1]'].includes(parsed.hostname);
	if (parsed.protocol !== 'https:' && !(parsed.protocol === 'http:' && local)) throw syncError('SYNC_NETWORK');
	if (parsed.username || parsed.password || parsed.search || parsed.hash) throw syncError('SYNC_NETWORK');
	parsed.pathname = parsed.pathname.replace(/\/+$/, '') || '/';
	return parsed.toString().replace(/\/$/, '');
}

export class SyncService {
	private busy = false;

	public constructor(private readonly adapter: SyncAdapter) {}

	public async getConfig(): Promise<SyncConfig> {
		const config = await this.adapter.readConfig();
		return config.configured ? { configured: true, url: config.url, username: config.username } : { configured: false };
	}

	public async configure(input: SyncConfigInput): Promise<SyncConfig> {
		const normalized: SyncConfigInput = {
			url: normalizeSyncUrl(input.url),
			username: input.username,
			password: input.password,
		};
		if (!normalized.username || normalized.username.length > 4096 || normalized.username.includes('\0') || !normalized.password || normalized.password.length > 4096 || normalized.password.includes('\0')) throw syncError('SYNC_AUTH_FAILED');
		try {
			await this.adapter.configure(normalized);
		} catch (error) {
			const code = error && typeof error === 'object' && 'code' in error && typeof error.code === 'string' ? error.code : 'SYNC_FAILED';
			throw syncError(code);
		}
		return this.getConfig();
	}

	public async syncNow(): Promise<SyncSummary> {
		if (this.busy) throw syncError('SYNC_BUSY');
		this.busy = true;
		try {
			return await this.adapter.syncNow();
		} catch (error) {
			const code = error && typeof error === 'object' && 'code' in error && typeof error.code === 'string' ? error.code : 'SYNC_FAILED';
			throw syncError(code);
		} finally {
			this.busy = false;
		}
	}
}
