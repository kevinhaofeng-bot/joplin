import { constants, closeSync, fstatSync, fsyncSync, lstatSync, openSync, readFileSync, writeSync } from 'node:fs';
import { lstat, readdir } from 'node:fs/promises';
import { join } from 'node:path';
import { profileError } from '../protocol';
import { revalidateProfilePath, revalidateProfilePathSync, type ValidatedProfilePaths } from './pathPolicy';

export const PROFILE_MARKER_CONTENT = '{"owner":"com.kevinhao.joplin-lite","formatVersion":1}';

// Node does not expose O_CLOEXEC in fs.constants; use the platform ABI value
// so marker handles cannot leak through an unrelated child process.
const O_CLOEXEC = (constants as typeof constants & { O_CLOEXEC?: number }).O_CLOEXEC ??
	(process.platform === 'darwin' ? 0x01000000 : 0x00080000);

function notOwned(): never {
	throw profileError('PROFILE_NOT_OWNED');
}

function existingOwnedMarker(path: string): boolean {
	let marker: number;
	try {
		marker = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW | O_CLOEXEC);
	} catch (error) {
		if ((error as NodeJS.ErrnoException).code === 'ENOENT') return false;
		notOwned();
	}

	let owned = false;
	let failed = false;
	try {
		const stat = fstatSync(marker);
		if (!stat.isFile()) notOwned();
		const pathStat = lstatSync(path);
		if (pathStat.isSymbolicLink() || !pathStat.isFile() || pathStat.dev !== stat.dev || pathStat.ino !== stat.ino) notOwned();
		owned = readFileSync(marker, { encoding: 'utf8' }) === PROFILE_MARKER_CONTENT;
	} catch {
		failed = true;
	} finally {
		try {
			closeSync(marker);
		} catch {
			failed = true;
		}
	}
	if (failed) notOwned();
	return owned;
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

function createMarker(path: string): void {
	let marker: number;
	try {
		marker = openSync(path, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW | O_CLOEXEC, 0o600);
	} catch (error) {
		if ((error as NodeJS.ErrnoException).code === 'EEXIST' && existingOwnedMarker(path)) return;
		notOwned();
	}

	let failed = false;
	try {
		const content = Buffer.from(PROFILE_MARKER_CONTENT, 'utf8');
		let offset = 0;
		while (offset < content.length) offset += writeSync(marker, content, offset, content.length - offset);
		fsyncSync(marker);
	} catch {
		failed = true;
	}
	try {
		closeSync(marker);
	} catch {
		failed = true;
	}
	if (failed) notOwned();
}

export async function claimProfile(paths: ValidatedProfilePaths): Promise<void> {
	let safePaths: ValidatedProfilePaths;
	try {
		safePaths = await revalidateProfilePath(paths);
	} catch (error) {
		if (error instanceof Error && 'code' in error && error.code === 'PROFILE_INVALID') throw error;
		throw profileError('PROFILE_INVALID');
	}

	const markerPath = join(safePaths.root, '.joplin-lite-profile.json');
	if (existingOwnedMarker(markerPath)) {
		await revalidateProfilePath(safePaths);
		return;
	}
	if (!(await isEmptyRustScaffold(safePaths))) notOwned();

	// Recheck after the asynchronous scaffold read, then keep the final
	// identity check and O_EXCL open in one synchronous critical section.
	safePaths = await revalidateProfilePath(safePaths);
	const finalPaths = revalidateProfilePathSync(safePaths);
	createMarker(join(finalPaths.root, '.joplin-lite-profile.json'));
	await revalidateProfilePath(finalPaths);
}
