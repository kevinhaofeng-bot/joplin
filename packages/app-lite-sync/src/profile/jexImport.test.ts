import { JexImportService, validateJexPath } from './jexImport';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';

describe('JEX import service', () => {
	let release!: ()=> void;

	test('rejects unsafe paths before starting an import', async () => {
		await expect(validateJexPath('/tmp/export.txt')).rejects.toMatchObject({ code: 'IMPORT_INVALID' });
		await expect(validateJexPath('relative.jex')).rejects.toMatchObject({ code: 'IMPORT_INVALID' });
		await expect(validateJexPath('/tmp/export\0.jex')).rejects.toMatchObject({ code: 'IMPORT_INVALID' });
	});

	test('returns running immediately and rejects concurrent imports', async () => {
		const dir = await mkdtemp(join(tmpdir(), 'joplin-lite-jex-'));
		const path = join(dir, 'export.jex');
		const second = join(dir, 'second.jex');
		await Promise.all([writeFile(path, 'fixture'), writeFile(second, 'fixture')]);
		const service = new JexImportService(async () => new Promise(resolve => {
			release = () => resolve({ notes: 1, folders: 1, tags: 0, resources: 0 });
		}));
		await expect(service.start(path)).resolves.toEqual({ state: 'running' });
		await expect(service.start(second)).rejects.toMatchObject({ code: 'IMPORT_BUSY' });
		release();
		await service.waitForIdle();
		expect(service.getStatus()).toEqual({ state: 'succeeded', summary: { notes: 1, folders: 1, tags: 0, resources: 0 } });
		await rm(dir, { recursive: true, force: true });
	});

	test('claims the importer after path validation when starts race', async () => {
		const dir = await mkdtemp(join(tmpdir(), 'joplin-lite-jex-race-'));
		const path = join(dir, 'export.jex');
		await writeFile(path, 'fixture');
		const service = new JexImportService(async () => new Promise(resolve => {
			release = () => resolve({ notes: 1, folders: 0, tags: 0, resources: 0 });
		}));
		const results = await Promise.allSettled([service.start(path), service.start(path)]);
		expect(results.filter(result => result.status === 'fulfilled')).toHaveLength(1);
		expect(results.filter(result => result.status === 'rejected')).toHaveLength(1);
		expect((results.find(result => result.status === 'rejected') as PromiseRejectedResult).reason).toMatchObject({ code: 'IMPORT_BUSY' });
		release();
		await service.waitForIdle();
		await rm(dir, { recursive: true, force: true });
	});
});
