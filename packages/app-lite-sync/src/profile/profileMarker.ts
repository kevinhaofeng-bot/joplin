import { lstat, open, readdir, readFile } from 'node:fs/promises';
import { profileError } from '../protocol';
import { validateProfilePath, type ValidatedProfilePaths } from './pathPolicy';

export const PROFILE_MARKER_CONTENT = '{"owner":"com.kevinhao.joplin-lite","formatVersion":1}';

function notOwned(): never {
	throw profileError('PROFILE_NOT_OWNED');
}

async function existingOwnedMarker(path: string): Promise<boolean> {
	let stat;
	try {
		stat = await lstat(path);
	} catch (error) {
		if ((error as NodeJS.ErrnoException).code === 'ENOENT') return false;
		notOwned();
	}
	if (stat.isSymbolicLink() || !stat.isFile()) notOwned();
	try {
		return (await readFile(path, 'utf8')) === PROFILE_MARKER_CONTENT;
	} catch {
		notOwned();
	}
}

async function isEmptyDirectory(path: string): Promise<boolean> {
	try {
		const stat = await lstat(path);
		if (stat.isSymbolicLink() || !stat.isDirectory()) return false;
		return (await readdir(path)).length === 0;
	} catch {
		return false;
	}
}

async function isEmptyRustScaffold(paths: ValidatedProfilePaths): Promise<boolean> {
	let entries;
	try {
		entries = await readdir(paths.root);
	} catch {
		return false;
	}
	if (entries.length !== 3 || new Set(entries).size !== 3) return false;
	return await Promise.all([
		isEmptyDirectory(paths.resources),
		isEmptyDirectory(paths.indexes),
		isEmptyDirectory(paths.logs),
	]).then(results => results.every(Boolean));
}

export async function claimProfile(paths: ValidatedProfilePaths): Promise<void> {
	let safePaths: ValidatedProfilePaths;
	try {
		safePaths = await validateProfilePath(paths.root);
	} catch (error) {
		if (error instanceof Error && 'code' in error && error.code === 'PROFILE_INVALID') throw error;
		throw profileError('PROFILE_INVALID');
	}

	if (await existingOwnedMarker(safePaths.marker)) return;
	if (!(await isEmptyRustScaffold(safePaths))) notOwned();

	let marker;
	try {
		marker = await open(safePaths.marker, 'wx', 0o600);
	} catch (error) {
		if ((error as NodeJS.ErrnoException).code === 'EEXIST' && await existingOwnedMarker(safePaths.marker)) return;
		notOwned();
	}
	try {
		await marker.writeFile(PROFILE_MARKER_CONTENT, 'utf8');
	} catch {
		notOwned();
	} finally {
		await marker.close().catch((): void => {});
	}
}
