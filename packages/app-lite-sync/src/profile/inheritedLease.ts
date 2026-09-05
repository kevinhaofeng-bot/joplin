import { closeSync, fstatSync, lstatSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { profileError } from '../protocol';
import { revalidateProfilePathSync, type ValidatedProfilePaths } from './pathPolicy';

export const INHERITED_LEASE_FD = 198 as const;
const LEASE_FILE_NAME = '.com.kevinhao.joplin-lite.canonical.lock';

export type InheritedLease = Readonly<{
	fd: typeof INHERITED_LEASE_FD;
	close: () => void;
}>;

function invalidLease(): never {
	throw profileError('PROFILE_LOCK_REQUIRED');
}

export function verifyInheritedLease(paths: ValidatedProfilePaths): InheritedLease {
	if (process.env.JOPLIN_LITE_PROFILE_LEASE_FD !== String(INHERITED_LEASE_FD)) invalidLease();
	let safePaths: ValidatedProfilePaths;
	try {
		safePaths = revalidateProfilePathSync(paths);
	} catch {
		invalidLease();
	}
	const lockPath = join(dirname(safePaths.root), LEASE_FILE_NAME);
	let descriptor: ReturnType<typeof fstatSync>;
	let lock: ReturnType<typeof lstatSync>;
	try {
		descriptor = fstatSync(INHERITED_LEASE_FD);
		lock = lstatSync(lockPath);
	} catch {
		invalidLease();
	}
	if (!descriptor.isFile() || lock.isSymbolicLink() || !lock.isFile() || descriptor.dev !== lock.dev || descriptor.ino !== lock.ino) invalidLease();
	let closed = false;
	return Object.freeze({
		fd: INHERITED_LEASE_FD,
		close: () => {
			if (closed) return;
			closed = true;
			try {
				closeSync(INHERITED_LEASE_FD);
			} catch {
				invalidLease();
			}
		},
	});
}
