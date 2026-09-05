import { lstat, mkdir, mkdtemp, rm, symlink, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { PROFILE_DIRECTORY_NAME, revalidateProfilePath, validateProfilePath } from './pathPolicy';

const INVALID = 'PROFILE_INVALID';
const tempParents = new Set<string>();

async function tempParent(): Promise<string> {
	const parent = await mkdtemp(join(tmpdir(), 'joplin-lite-policy-'));
	tempParents.add(parent);
	return parent;
}

async function profileRoot(parent: string | undefined = undefined, name = PROFILE_DIRECTORY_NAME): Promise<string> {
	const root = join(parent ?? await tempParent(), name);
	await mkdir(root);
	return root;
}

async function expectInvalid(path: unknown): Promise<void> {
	await expect(validateProfilePath(path)).rejects.toMatchObject({ code: INVALID, message: '资料库路径无效' });
}

describe('validateProfilePath', () => {
	afterEach(async () => {
		for (const parent of tempParents) await rm(parent, { recursive: true, force: true });
		tempParents.clear();
	});

	test.each([
		['relative path', 'com.kevinhao.joplin-lite'],
		['wrong basename', 'not-joplin-lite'],
	])('rejects %s', async (_label, name) => {
		const parent = await tempParent();
		const path = _label === 'relative path' ? `relative/${name}` : await profileRoot(parent, name);
		await expectInvalid(path);
	});

	test('rejects a legacy component regardless of case', async () => {
		const parent = await tempParent();
		const legacy = join(parent, 'JoPlIn-DeSkToP');
		await mkdir(legacy);
		await expectInvalid(await profileRoot(legacy));
	});

	test('rejects a canonical parent whose real path contains the legacy component', async () => {
		const parent = await tempParent();
		const legacy = join(parent, 'joplin-desktop');
		const link = join(parent, 'safe-parent');
		await mkdir(legacy);
		await symlink(legacy, link);
		await expectInvalid(await profileRoot(link));
	});

	test('rejects a symlink profile root', async () => {
		const parent = await tempParent();
		const target = await profileRoot(parent, 'target');
		const root = join(parent, PROFILE_DIRECTORY_NAME);
		await symlink(target, root);
		await expectInvalid(root);
	});

	test('accepts a real profile root with the three Rust scaffold directories', async () => {
		const root = await profileRoot();
		await Promise.all(['resources', 'indexes', 'logs'].map(name => mkdir(join(root, name))));
		const paths = await validateProfilePath(root);
		expect(paths).toMatchObject({ root, resources: join(root, 'resources'), indexes: join(root, 'indexes'), logs: join(root, 'logs') });
	});

	test.each([
		['resources', 'directory'], ['indexes', 'directory'], ['logs', 'directory'], ['tmp', 'directory'], ['cache', 'directory'],
		['database.sqlite', 'file'], ['database.sqlite-journal', 'file'], ['database.sqlite-wal', 'file'], ['database.sqlite-shm', 'file'],
		['settings.json', 'file'], ['.joplin-lite-profile.json', 'file'],
	])('rejects a symlink %s child', async (name, kind) => {
		const root = await profileRoot();
		await Promise.all(['resources', 'indexes', 'logs'].map(scaffold => mkdir(join(root, scaffold))));
		await rm(join(root, name), { recursive: true, force: true });
		const target = join(root, `target-${name}`);
		if (kind === 'directory') await mkdir(target);
		else await writeFile(target, 'fixture');
		await symlink(target, join(root, name));
		await expectInvalid(root);
	});

	test.each([
		['resources', 'directory'], ['indexes', 'directory'], ['logs', 'directory'], ['tmp', 'directory'], ['cache', 'directory'],
		['database.sqlite', 'file'], ['database.sqlite-journal', 'file'], ['database.sqlite-wal', 'file'], ['database.sqlite-shm', 'file'],
		['settings.json', 'file'], ['.joplin-lite-profile.json', 'file'],
	])('rejects a wrong type for %s', async (name, expectedKind) => {
		const root = await profileRoot();
		await Promise.all(['resources', 'indexes', 'logs'].map(scaffold => mkdir(join(root, scaffold))));
		await rm(join(root, name), { recursive: true, force: true });
		if (expectedKind === 'directory') await writeFile(join(root, name), 'wrong type');
		else await mkdir(join(root, name));
		await expectInvalid(root);
	});

	test('never includes a rejected path in its fixed error', async () => {
		const marker = 'path-secret-marker';
		await expect(validateProfilePath(join(tmpdir(), `${marker}-wrong`))).rejects.toMatchObject({ code: INVALID });
		await expect(validateProfilePath(join(tmpdir(), `${marker}-wrong`))).rejects.not.toHaveProperty('message', expect.stringContaining(marker));
	});

	test('does not follow a symlink while checking child types', async () => {
		const root = await profileRoot();
		await Promise.all(['resources', 'indexes', 'logs'].map(name => mkdir(join(root, name))));
		const target = join(root, 'outside');
		await mkdir(target);
		await symlink(target, join(root, 'tmp'));
		await expectInvalid(root);
		await expect(lstat(target)).resolves.toMatchObject({ isDirectory: expect.any(Function) });
	});

	test('revalidates critical profile paths for a later SQLite boundary', async () => {
		const root = await profileRoot();
		await Promise.all(['resources', 'indexes', 'logs'].map(name => mkdir(join(root, name))));
		const paths = await validateProfilePath(root);
		await expect(revalidateProfilePath(paths)).resolves.toEqual(paths);
		await rm(paths.resources, { recursive: true, force: true });
		await symlink(paths.logs, paths.resources);
		await expect(revalidateProfilePath(paths)).rejects.toMatchObject({ code: INVALID, message: '资料库路径无效' });
	});
});
