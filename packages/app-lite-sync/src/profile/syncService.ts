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

export type SyncCode = 'SYNC_NOT_CONFIGURED' | 'SYNC_AUTH_FAILED' | 'SYNC_NETWORK' | 'SYNC_BUSY' | 'SYNC_FAILED';
export type SyncStatus =
	| { state: 'idle' | 'running' }
	| { state: 'succeeded'; summary: SyncSummary }
	| { state: 'failed'; code: Exclude<SyncCode, 'SYNC_BUSY'> };

export type SyncAdapter = {
	readConfig: ()=> Promise<SyncConfig>;
	configure: (input: SyncConfigInput)=> Promise<void>;
	syncNow: ()=> Promise<SyncSummary>;
};

export type SyncKeytar = {
	getPassword: (service: string, account: string)=> Promise<string|null>;
	setPassword: (service: string, account: string, password: string)=> Promise<void>;
	deletePassword: (service: string, account: string)=> Promise<void>;
};

export type SyncSecretStore = {
	read: ()=> Promise<string|null>;
	write: (password: string)=> Promise<boolean>;
	remove: ()=> Promise<void>;
};

export function createSyncSecretStore(keytar: SyncKeytar|null, service: string, account: string): SyncSecretStore|null {
	if (!keytar) return null;
	return {
		read: () => keytar.getPassword(service, account),
		write: async password => {
			await keytar.setPassword(service, account, password);
			return true;
		},
		remove: () => keytar.deletePassword(service, account),
	};
}

export function syncConfigFromMetadata(target: unknown, targetId: unknown, url: unknown, username: unknown): SyncConfig {
	const configured = target === targetId
		&& typeof url === 'string' && url.length > 0
		&& typeof username === 'string' && username.length > 0;
	return configured ? { configured: true, url: url as string, username: username as string } : { configured: false };
}

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
	private status: SyncStatus = { state: 'idle' };
	private active: { result: Promise<SyncSummary>; settled: Promise<void> } | undefined;

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

	public getSyncStatus(): SyncStatus {
		if (this.status.state === 'succeeded') return { state: 'succeeded', summary: { ...this.status.summary } };
		return this.status;
	}

	public async startSync(): Promise<SyncStatus> {
		this.beginSync();
		return this.getSyncStatus();
	}

	public async waitForIdle(): Promise<void> {
		await this.active?.settled;
	}

	public async syncNow(): Promise<SyncSummary> {
		return this.beginSync();
	}

	private beginSync(): Promise<SyncSummary> {
		if (this.active) throw syncError('SYNC_BUSY');
		this.status = { state: 'running' };
		const result = Promise.resolve().then(() => this.adapter.syncNow()).then(summary => {
			this.status = { state: 'succeeded', summary };
			return summary;
		}, error => {
			const code = error && typeof error === 'object' && 'code' in error && typeof error.code === 'string' ? error.code : 'SYNC_FAILED';
			const fixed = syncError(code);
			this.status = { state: 'failed', code: fixed.code as Exclude<SyncCode, 'SYNC_BUSY'> };
			throw fixed;
		});
		const settled = result.then((): void => undefined, (): void => undefined);
		this.active = { result, settled };
		void result.catch((): void => undefined);
		void settled.then(() => {
			if (this.active?.settled === settled) this.active = undefined;
		});
		return result;
	}
}
