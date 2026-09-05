import { lstatSync, realpathSync } from 'node:fs';
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
type ValidationState = {
	facade: ValidatedProfilePaths;
	root: string;
	snapshot: IdentitySnapshot;
	generation: number;
	queue: Promise<void>;
	inFlight: boolean;
};

const states = new WeakMap<object, ValidationState>();

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

function safeLstatSync(path: string): ReturnType<typeof lstatSync> | undefined {
	try {
		return lstatSync(path);
	} catch (error) {
		if ((error as NodeJS.ErrnoException).code === 'ENOENT') return undefined;
		invalid();
	}
}

function requireDirectory(stat: { isSymbolicLink(): boolean; isDirectory(): boolean } | undefined): void {
	if (!stat || stat.isSymbolicLink() || !stat.isDirectory()) invalid();
}

function identity(stat: { dev: number | bigint; ino: number | bigint }, type: EntryType): Identity {
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

async function scanProfilePath(root: string): Promise<IdentitySnapshot> {
	const rootStat = await safeLstat(root);
	requireDirectory(rootStat);

	let rootRealPath: string;
	let canonicalParent: string;
	try {
		rootRealPath = await realpath(root);
		canonicalParent = await realpath(dirname(root));
	} catch {
		invalid();
	}
	if (includesLegacyComponent(canonicalParent)) invalid();
	const canonicalParentStat = await safeLstat(canonicalParent);
	requireDirectory(canonicalParentStat);

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
	return {
		root,
		rootRealPath,
		rootIdentity: identity(rootStat, 'directory'),
		parentRealPath: canonicalParent,
		parentIdentity: identity(canonicalParentStat, 'directory'),
		entries,
	};
}

function scanProfilePathSync(root: string): IdentitySnapshot {
	const rootStat = safeLstatSync(root);
	requireDirectory(rootStat);

	let rootRealPath: string;
	let canonicalParent: string;
	try {
		rootRealPath = realpathSync(root);
		canonicalParent = realpathSync(dirname(root));
	} catch {
		invalid();
	}
	if (includesLegacyComponent(canonicalParent)) invalid();
	const canonicalParentStat = safeLstatSync(canonicalParent);
	requireDirectory(canonicalParentStat);

	const entries = new Map<string, Identity>();
	for (const name of directories) {
		const path = join(root, name);
		const stat = safeLstatSync(path);
		if (stat && (stat.isSymbolicLink() || !stat.isDirectory())) invalid();
		if (stat) entries.set(path, identity(stat, 'directory'));
	}
	for (const name of files) {
		const path = join(root, name);
		const stat = safeLstatSync(path);
		if (stat && (stat.isSymbolicLink() || !stat.isFile())) invalid();
		if (stat) entries.set(path, identity(stat, 'file'));
	}
	return {
		root,
		rootRealPath,
		rootIdentity: identity(rootStat, 'directory'),
		parentRealPath: canonicalParent,
		parentIdentity: identity(canonicalParentStat, 'directory'),
		entries,
	};
}

function advanceState(state: ValidationState, current: IdentitySnapshot): void {
	const previous = state.snapshot;
	if (current.rootRealPath !== previous.rootRealPath || current.parentRealPath !== previous.parentRealPath ||
		!sameIdentity(current.rootIdentity, previous.rootIdentity) || !sameIdentity(current.parentIdentity, previous.parentIdentity)) invalid();
	for (const [path, previousIdentity] of previous.entries) {
		const currentIdentity = current.entries.get(path);
		if (!currentIdentity || !sameIdentity(currentIdentity, previousIdentity)) invalid();
	}
	state.snapshot = current;
	state.generation += 1;
}

export async function validateProfilePath(input: unknown): Promise<ValidatedProfilePaths> {
	const root = rawProfilePath(input);
	const snapshot = await scanProfilePath(root);
	const facade = Object.freeze(pathsFor(root));
	const state: ValidationState = { facade, root, snapshot, generation: 0, queue: Promise.resolve(), inFlight: false };
	states.set(facade, state);
	return facade;
}

export async function revalidateProfilePath(paths: ValidatedProfilePaths): Promise<ValidatedProfilePaths> {
	const state = states.get(paths as object);
	if (!state) invalid();
	const run = state.queue.then(async () => {
		state.inFlight = true;
		try {
			advanceState(state, await scanProfilePath(state.root));
			return state.facade;
		} finally {
			state.inFlight = false;
		}
	});
	state.queue = run.then((): void => undefined, (): void => undefined);
	return run;
}

/** Final synchronous identity check used immediately before marker O_EXCL open. */
export function revalidateProfilePathSync(paths: ValidatedProfilePaths): ValidatedProfilePaths {
	const state = states.get(paths as object);
	if (!state) invalid();
	advanceState(state, scanProfilePathSync(state.root));
	return state.facade;
}
