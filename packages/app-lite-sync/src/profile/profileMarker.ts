import { constants } from 'node:fs';
import { lstat, open, readdir } from 'node:fs/promises';
import { profileError } from '../protocol';
import { revalidateProfilePath, type ValidatedProfilePaths } from './pathPolicy';

export const PROFILE_MARKER_CONTENT = '{"owner":"com.kevinhao.joplin-lite","formatVersion":1}';

// Node does not expose O_CLOEXEC in fs.constants; use the platform ABI value
// so marker handles cannot leak through an unrelated child process.
const O_CLOEXEC = (constants as typeof constants & { O_CLOEXEC?: number }).O_CLOEXEC ??
	(process.platform === 'darwin' ? 0x01000000 : 0x00080000);

function notOwned(): never {
	throw profileError('PROFILE_NOT_OWNED');
}

async function existingOwnedMarker(path: string): Promise<boolean> {
	let marker;
	try {
		marker = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW | O_CLOEXEC);
	} catch (error) {
		if ((error as NodeJS.ErrnoException).code === 'ENOENT') return false;
		notOwned();
	}
	try {
		const stat = await marker.stat();
		if (stat.isSymbolicLink() || !stat.isFile()) notOwned();
		return (await marker.readFile({ encoding: 'utf8' })) === PROFILE_MARKER_CONTENT;
	} catch {
		notOwned();
	} finally {
		await marker.close().catch((): void => {});
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
		safePaths = await revalidateProfilePath(paths);
	} catch (error) {
		if (error instanceof Error && 'code' in error && error.code === 'PROFILE_INVALID') throw error;
		throw profileError('PROFILE_INVALID');
	}

	if (await existingOwnedMarker(safePaths.marker)) return;
	if (!(await isEmptyRustScaffold(safePaths))) notOwned();

	let marker;
	try {
		marker = await open(safePaths.marker, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW | O_CLOEXEC, 0o600);
	} catch (error) {
		if ((error as NodeJS.ErrnoException).code === 'EEXIST' && await existingOwnedMarker(safePaths.marker)) return;
		notOwned();
	}
	try {
		await marker.writeFile(PROFILE_MARKER_CONTENT, 'utf8');
		await marker.sync();
	} catch {
		notOwned();
	} finally {
		await marker.close().catch((): void => {});
	}
}
