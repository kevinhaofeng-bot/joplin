import { lstat, realpath } from 'node:fs/promises';
import { basename, dirname, isAbsolute, resolve, join, sep } from 'node:path';
import { PROFILE_DIRECTORY_NAME, profileError } from '../protocol';

export { PROFILE_DIRECTORY_NAME } from '../protocol';

export type ValidatedProfilePaths = Readonly<{
	root: string;
	database: string;
	resources: string;
	indexes: string;
	logs: string;
	settings: string;
	marker: string;
	temp: string;
	cache: string;
}>;

const directories = ['resources', 'indexes', 'logs', 'tmp', 'cache'] as const;
const files = ['database.sqlite', 'database.sqlite-journal', 'database.sqlite-wal', 'database.sqlite-shm', 'settings.json', '.joplin-lite-profile.json'] as const;

function invalid(): never {
	throw profileError('PROFILE_INVALID');
}

function includesLegacyComponent(path: string): boolean {
	return path.split(sep).some(component => component.toLowerCase() === 'joplin-desktop');
}

async function safeLstat(path: string): Promise<Awaited<ReturnType<typeof lstat>> | undefined> {
	try {
		return await lstat(path);
	} catch (error) {
		if ((error as NodeJS.ErrnoException).code === 'ENOENT') return undefined;
		invalid();
	}
}

function requireDirectory(stat: Awaited<ReturnType<typeof lstat>> | undefined): void {
	if (!stat || stat.isSymbolicLink() || !stat.isDirectory()) invalid();
}

export async function validateProfilePath(input: unknown): Promise<ValidatedProfilePaths> {
	if (typeof input !== 'string' || !isAbsolute(input)) invalid();
	const root = resolve(input);
	if (basename(root) !== PROFILE_DIRECTORY_NAME || includesLegacyComponent(root)) invalid();

	const rootStat = await safeLstat(root);
	requireDirectory(rootStat);

	const parent = dirname(root);
	let canonicalParent: string;
	try {
		canonicalParent = await realpath(parent);
	} catch {
		invalid();
	}
	if (includesLegacyComponent(canonicalParent)) invalid();

	for (const name of directories) {
		const stat = await safeLstat(join(root, name));
		if (stat && (stat.isSymbolicLink() || !stat.isDirectory())) invalid();
	}
	for (const name of files) {
		const stat = await safeLstat(join(root, name));
		if (stat && (stat.isSymbolicLink() || !stat.isFile())) invalid();
	}

	return {
		root,
		database: join(root, 'database.sqlite'),
		resources: join(root, 'resources'),
		indexes: join(root, 'indexes'),
		logs: join(root, 'logs'),
		settings: join(root, 'settings.json'),
		marker: join(root, '.joplin-lite-profile.json'),
		temp: join(root, 'tmp'),
		cache: join(root, 'cache'),
	};
}

export async function revalidateProfilePath(paths: ValidatedProfilePaths): Promise<ValidatedProfilePaths> {
	const current = await validateProfilePath(paths.root);
	for (const key of ['root', 'database', 'resources', 'indexes', 'logs', 'settings', 'marker', 'temp', 'cache'] as const) {
		if (current[key] !== paths[key]) invalid();
	}
	return current;
}
