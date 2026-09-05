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

type EntryType = 'file' | 'directory';
type Identity = { dev: number | bigint; ino: number | bigint; type: EntryType };
type IdentitySnapshot = {
	root: string;
	rootRealPath: string;
	rootIdentity: Identity;
	parentRealPath: string;
	parentIdentity: Identity;
	entries: Map<string, Identity>;
};

const snapshots = new WeakMap<object, IdentitySnapshot>();

const directories = ['resources', 'indexes', 'logs', 'tmp', 'cache'] as const;
const files = ['database.sqlite', 'database.sqlite-journal', 'database.sqlite-wal', 'database.sqlite-shm', 'settings.json', '.joplin-lite-profile.json'] as const;

function invalid(): never {
	throw profileError('PROFILE_INVALID');
}

function includesLegacyComponent(path: string): boolean {
	return path.split(sep).some(component => component.toLowerCase() === 'joplin-desktop');
}

function rawProfilePath(input: unknown): string {
	if (typeof input !== 'string' || !isAbsolute(input)) invalid();
	const components = input.split(sep);
	if (basename(input) !== PROFILE_DIRECTORY_NAME || components.some(component => component === '..' || component.toLowerCase() === 'joplin-desktop')) invalid();
	return resolve(input);
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

function identity(stat: Awaited<ReturnType<typeof lstat>>, type: EntryType): Identity {
	return { dev: stat.dev, ino: stat.ino, type };
}

function sameIdentity(left: Identity, right: Identity): boolean {
	return left.dev === right.dev && left.ino === right.ino && left.type === right.type;
}

function pathsFor(root: string): ValidatedProfilePaths {
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

export async function validateProfilePath(input: unknown): Promise<ValidatedProfilePaths> {
	const root = rawProfilePath(input);

	const rootStat = await safeLstat(root);
	requireDirectory(rootStat);

	const parent = dirname(root);
	let rootRealPath: string;
	let canonicalParent: string;
	try {
		rootRealPath = await realpath(root);
		canonicalParent = await realpath(parent);
	} catch {
		invalid();
	}
	if (includesLegacyComponent(canonicalParent)) invalid();
	const canonicalParentStat = await safeLstat(canonicalParent);
	requireDirectory(canonicalParentStat);

	const paths = pathsFor(root);
	const entries = new Map<string, Identity>();
	for (const name of directories) {
		const path = join(root, name);
		const stat = await safeLstat(path);
		if (stat && (stat.isSymbolicLink() || !stat.isDirectory())) invalid();
		if (stat) entries.set(path, identity(stat, 'directory'));
	}
	for (const name of files) {
		const path = join(root, name);
		const stat = await safeLstat(path);
		if (stat && (stat.isSymbolicLink() || !stat.isFile())) invalid();
		if (stat) entries.set(path, identity(stat, 'file'));
	}

	const snapshot: IdentitySnapshot = {
		root,
		rootRealPath,
		rootIdentity: identity(rootStat, 'directory'),
		parentRealPath: canonicalParent,
		parentIdentity: identity(canonicalParentStat, 'directory'),
		entries,
	};
	snapshots.set(paths, snapshot);
	return paths;
}

export async function revalidateProfilePath(paths: ValidatedProfilePaths): Promise<ValidatedProfilePaths> {
	const original = snapshots.get(paths as object);
	if (!original) invalid();
	const current = await validateProfilePath(original.root);
	const currentSnapshot = snapshots.get(current as object);
	if (!currentSnapshot || currentSnapshot.rootRealPath !== original.rootRealPath || currentSnapshot.parentRealPath !== original.parentRealPath ||
		!sameIdentity(currentSnapshot.rootIdentity, original.rootIdentity) || !sameIdentity(currentSnapshot.parentIdentity, original.parentIdentity)) invalid();
	for (const [path, previousIdentity] of original.entries) {
		const currentIdentity = currentSnapshot.entries.get(path);
		if (!currentIdentity || !sameIdentity(currentIdentity, previousIdentity)) invalid();
	}
	// New managed entries (including SQLite auxiliary files) may appear between
	// validation phases; once observed, their identity is pinned for later calls.
	snapshots.set(paths as object, currentSnapshot);
	return current;
}
