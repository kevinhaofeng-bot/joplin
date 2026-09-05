import { SyncService, type SyncAdapter } from './syncService';

function adapter(overrides: Partial<SyncAdapter> = {}): SyncAdapter {
	return {
		readConfig: async () => ({ configured: true, url: 'https://old.example.test', username: 'old@example.test' }),
		configure: jest.fn(async (): Promise<void> => undefined),
		syncNow: jest.fn(async () => ({ completedAt: 1, created: 0, updated: 0, deleted: 0, fetched: 0 })),
		...overrides,
	};
}

describe('SyncService', () => {
	test('never exposes the configured password', async () => {
		const service = new SyncService(adapter());
		expect(await service.getConfig()).toEqual({ configured: true, url: 'https://old.example.test', username: 'old@example.test' });
		expect(JSON.stringify(await service.getConfig())).not.toContain('secret-password');
	});

	test('keeps the previous configuration when validation fails', async () => {
		const backend = adapter({ configure: jest.fn(async () => { throw Object.assign(new Error('remote secret'), { code: 'SYNC_AUTH_FAILED' }); }) });
		const service = new SyncService(backend);
		await expect(service.configure({ url: 'https://new.example.test/', username: 'new@example.test', password: 'secret-password' })).rejects.toMatchObject({ code: 'SYNC_AUTH_FAILED' });
		expect(backend.configure).toHaveBeenCalledWith({ url: 'https://new.example.test', username: 'new@example.test', password: 'secret-password' });
		expect(await service.getConfig()).toEqual({ configured: true, url: 'https://old.example.test', username: 'old@example.test' });
	});

	test('serializes concurrent sync requests as a stable busy error', async () => {
		let release!: ()=> void;
		const backend = adapter({ syncNow: jest.fn(() => new Promise(resolve => { release = () => resolve({ completedAt: 2, created: 1, updated: 0, deleted: 0, fetched: 0 }); })) });
		const service = new SyncService(backend);
		const first = service.syncNow();
		await expect(service.syncNow()).rejects.toMatchObject({ code: 'SYNC_BUSY' });
		release();
		await expect(first).resolves.toEqual({ completedAt: 2, created: 1, updated: 0, deleted: 0, fetched: 0 });
	});
});
