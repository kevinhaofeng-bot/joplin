import { decodeItem, encodeItem, registerItemClasses } from './codec';
import {
	failureFrame,
	profileError,
	PROFILE_ERROR_MESSAGES,
	ProtocolError,
	successFrame,
	type RequestFrame,
	type ResponseFrame,
} from './protocol';
import type { ProfileSession } from './profile/profileSession';

const joplinVersion: string = require('../../lib/package.json').version;
const capabilities = ['decodeItem', 'encodeItem', 'shutdown', 'profileStatus', 'openProfile'] as const;
const terminalCodes = new Set(['PROFILE_LOCK_REQUIRED', 'PROFILE_OPEN_FAILED', 'STORAGE_ERROR']);
const stableCodes = new Set(['INVALID_ITEM', 'INVALID_REQUEST', ...Object.keys(PROFILE_ERROR_MESSAGES)]);

type HandledRequest = { response: ResponseFrame; shouldExit: boolean };
export type RequestHandler = { handleRequest(request: RequestFrame): Promise<HandledRequest> };
type SessionLike = Pick<ProfileSession, 'status' | 'open' | 'flush' | 'close'>;

function invalidRequest(): ProtocolError {
	return new ProtocolError('INVALID_REQUEST', '请求格式无效');
}

function isObject(value: unknown): value is Record<string, unknown> {
	return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function hasOnly(params: Record<string, unknown>, keys: readonly string[]): boolean {
	return Object.keys(params).length === keys.length && keys.every(key => Object.prototype.hasOwnProperty.call(params, key));
}

function fixedError(error: unknown, fallback: string): ProtocolError {
	if (error instanceof ProtocolError && stableCodes.has(error.code)) {
		if (Object.prototype.hasOwnProperty.call(PROFILE_ERROR_MESSAGES, error.code)) return profileError(error.code as keyof typeof PROFILE_ERROR_MESSAGES);
		if (error.code === 'INVALID_ITEM') return new ProtocolError('INVALID_ITEM', 'Joplin 项目格式无效');
		return new ProtocolError('INVALID_REQUEST', '请求格式无效');
	}
	if (fallback === 'INVALID_REQUEST') return new ProtocolError('INVALID_REQUEST', '请求格式无效');
	return new ProtocolError(fallback, fallback === 'STORAGE_ERROR' ? PROFILE_ERROR_MESSAGES.STORAGE_ERROR : 'Joplin 项目格式无效');
}

export function createHandler(session: SessionLike): RequestHandler {
	return {
		handleRequest: async (request: RequestFrame): Promise<HandledRequest> => {
			try {
				if (!['hello', 'profileStatus', 'openProfile', 'shutdown'].includes(request.command)) registerItemClasses();
				switch (request.command) {
				case 'hello':
					if (!hasOnly(request.params, [])) throw invalidRequest();
					return { response: successFrame(request.id, { protocolVersion: 1, joplinVersion, capabilities }), shouldExit: false };
				case 'profileStatus':
					if (!hasOnly(request.params, [])) throw invalidRequest();
					return { response: successFrame(request.id, session.status()), shouldExit: false };
				case 'openProfile':
					if (!hasOnly(request.params, ['profilePath']) || typeof request.params.profilePath !== 'string') throw invalidRequest();
					return { response: successFrame(request.id, await session.open(request.params.profilePath)), shouldExit: false };
				case 'decodeItem':
					if (typeof request.params.raw !== 'string') throw invalidRequest();
					return { response: successFrame(request.id, await decodeItem(request.params.raw)), shouldExit: false };
				case 'encodeItem':
					if (!isObject(request.params.item)) throw invalidRequest();
					return { response: successFrame(request.id, await encodeItem(request.params.item)), shouldExit: false };
				case 'shutdown':
					if (!hasOnly(request.params, [])) throw invalidRequest();
					await session.close();
					return { response: successFrame(request.id, { stopped: true }), shouldExit: true };
				default:
					return { response: failureFrame(request.id, 'UNKNOWN_COMMAND', '未知命令'), shouldExit: false };
				}
			} catch (error) {
				const fixed = error instanceof ProtocolError ? fixedError(error, 'INVALID_REQUEST') : new ProtocolError('INVALID_ITEM', 'Joplin 项目格式无效');
				return { response: failureFrame(request.id, fixed.code, fixed.message), shouldExit: terminalCodes.has(fixed.code) };
			}
		},
	};
}
