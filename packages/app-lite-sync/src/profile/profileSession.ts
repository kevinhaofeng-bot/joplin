import { claimProfile } from './profileMarker';
import { revalidateProfilePath, validateProfilePath, type ValidatedProfilePaths } from './pathPolicy';
import { verifyInheritedLease, type InheritedLease } from './inheritedLease';
import { openJoplinRuntime, type RuntimeHandle } from './joplinRuntime';
import { profileError, ProtocolError } from '../protocol';

export type ProfileState = 'closed' | 'open';
export type ProfileStatus = { state: ProfileState; formatVersion: 1 };
export type OpenProfileResult = { state: 'open'; schemaVersion: number; formatVersion: 1 };

export interface ProfileSession {
	status(): ProfileStatus;
	requireOpen(): void;
	open(profilePath: unknown): Promise<OpenProfileResult>;
	flush(): Promise<void>;
	close(): Promise<void>;
}

export type ProfileRuntimeFactory = (paths: ValidatedProfilePaths)=> Promise<RuntimeHandle>;
export type ProfileLeaseVerifier = (paths: ValidatedProfilePaths)=> InheritedLease;

type SessionOptions = {
	runtimeFactory?: ProfileRuntimeFactory;
	verifyLease?: ProfileLeaseVerifier;
};

function errorCode(error: unknown): string | undefined {
	return error && typeof error === 'object' && 'code' in error && typeof error.code === 'string' ? error.code : undefined;
}

export class ProfileSession implements ProfileSession {
	private state: ProfileState = 'closed';
	private terminalError: ProtocolError | undefined;
	private runtime: RuntimeHandle | undefined;
	private lease: InheritedLease | undefined;
	private leaseRoot: string | undefined;
	private readonly runtimeFactory: ProfileRuntimeFactory;
	private readonly verifyLease: ProfileLeaseVerifier;

	public constructor(options: SessionOptions = {}) {
		this.runtimeFactory = options.runtimeFactory ?? openJoplinRuntime;
		this.verifyLease = options.verifyLease ?? verifyInheritedLease;
	}

	public status(): ProfileStatus {
		return { state: this.state, formatVersion: 1 };
	}

	public requireOpen(): void {
		if (!this.runtime || this.state !== 'open') throw profileError('PROFILE_NOT_OPEN');
	}

	public async open(profilePath: unknown): Promise<OpenProfileResult> {
		if (this.state === 'open') throw profileError('PROFILE_ALREADY_OPEN');
		if (this.terminalError) throw this.terminalError;

		let paths: ValidatedProfilePaths;
		try {
			paths = await validateProfilePath(profilePath);
		} catch (error) {
			throw this.fixedDomainError(error, 'PROFILE_INVALID');
		}

		if (!this.lease) {
			try {
				this.lease = this.verifyLease(paths);
				this.leaseRoot = paths.root;
			} catch (error) {
				const fixed = this.fixedDomainError(error, 'PROFILE_LOCK_REQUIRED');
				if (fixed.code === 'PROFILE_LOCK_REQUIRED') this.terminalError = fixed;
				throw fixed;
			}
		} else if (this.leaseRoot !== paths.root) {
			const fixed = profileError('PROFILE_LOCK_REQUIRED');
			this.terminalError = fixed;
			await this.cleanupPartialOpen();
			throw fixed;
		}

		let runtimeStarted = false;
		try {
			await claimProfile(paths);
			paths = await revalidateProfilePath(paths);
			runtimeStarted = true;
			const runtime = await this.runtimeFactory(paths);
			this.runtime = runtime;
			await revalidateProfilePath(paths);
			this.state = 'open';
			return { state: 'open', schemaVersion: runtime.schemaVersion, formatVersion: 1 };
		} catch (error) {
			const code = errorCode(error);
			if (!runtimeStarted && (code === 'PROFILE_INVALID' || code === 'PROFILE_NOT_OWNED')) {
				this.state = 'closed';
				throw this.fixedDomainError(error, code);
			}
			await this.cleanupPartialOpen();
			const fixed = code === 'PROFILE_LOCK_REQUIRED' ? this.fixedDomainError(error, 'PROFILE_LOCK_REQUIRED') : profileError('PROFILE_OPEN_FAILED');
			this.terminalError = fixed;
			throw fixed;
		}
	}

	public async flush(): Promise<void> {
		if (!this.runtime) throw profileError('PROFILE_NOT_OPEN');
		try {
			await this.runtime.flush();
		} catch {
			throw profileError('STORAGE_ERROR');
		}
	}

	public async close(): Promise<void> {
		if (!this.runtime && !this.lease) return;
		let failed = false;
		if (this.runtime) {
			try {
				await this.runtime.close();
			} catch {
				failed = true;
			}
			this.runtime = undefined;
		}
		if (this.lease) {
			try {
				this.lease.close();
			} catch {
				failed = true;
			}
			this.lease = undefined;
			this.leaseRoot = undefined;
		}
		this.state = 'closed';
		if (failed) {
			const error = profileError('STORAGE_ERROR');
			this.terminalError = error;
			throw error;
		}
	}

	private async cleanupPartialOpen(): Promise<void> {
		try {
			if (this.runtime) await this.runtime.close();
		} catch {
			// Continue to release the inherited lease even if runtime cleanup fails.
		}
		this.runtime = undefined;
		try {
			this.lease?.close();
		} catch {
			// The terminal error remains fixed and never includes OS details.
		}
		this.lease = undefined;
		this.leaseRoot = undefined;
		this.state = 'closed';
	}

	private fixedDomainError(error: unknown, fallback: 'PROFILE_INVALID' | 'PROFILE_NOT_OWNED' | 'PROFILE_LOCK_REQUIRED'): ProtocolError {
		const code = errorCode(error);
		if (code === 'PROFILE_INVALID' || code === 'PROFILE_NOT_OWNED' || code === 'PROFILE_LOCK_REQUIRED') return profileError(code);
		return profileError(fallback);
	}
}
