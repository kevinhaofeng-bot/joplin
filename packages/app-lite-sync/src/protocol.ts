export const PROTOCOL_VERSION = 1 as const;
export const MAX_FRAME_BYTES = 8 * 1024 * 1024;
export const PROFILE_DIRECTORY_NAME = 'com.kevinhao.joplin-lite';
export const PROFILE_FORMAT_VERSION = 1 as const;
export const PROFILE_ERROR_MESSAGES = {
	PROFILE_INVALID: '资料库路径无效',
	PROFILE_NOT_OWNED: '资料库不属于 Joplin Lite',
	PROFILE_IN_USE: '资料库正在被使用',
	PROFILE_LOCK_REQUIRED: '资料库写入租约无效',
	PROFILE_NOT_OPEN: '资料库尚未打开',
	PROFILE_ALREADY_OPEN: '资料库已经打开',
	PROFILE_OPEN_FAILED: '无法打开资料库',
	STORAGE_ERROR: '无法保存资料库',
} as const;

export type ProfileErrorCode = keyof typeof PROFILE_ERROR_MESSAGES;

export type RequestFrame = {
	id: string;
	protocolVersion: typeof PROTOCOL_VERSION;
	command: string;
	params: Record<string, unknown>;
};

export type SuccessFrame = { id: string; ok: true; result: unknown };
export type FailureFrame = {
	id: string;
	ok: false;
	error: { code: string; message: string };
};
export type ResponseFrame = SuccessFrame | FailureFrame;

export class ProtocolError extends Error {
	public constructor(public readonly code: string, message: string) {
		super(message);
		this.name = 'ProtocolError';
	}
}

export function profileError(code: ProfileErrorCode): ProtocolError {
	return new ProtocolError(code, PROFILE_ERROR_MESSAGES[code]);
}

const invalidRequest = (): ProtocolError => new ProtocolError('INVALID_REQUEST', '请求格式无效');

export function parseRequestFrame(line: string): RequestFrame {
	if (typeof line !== 'string') throw invalidRequest();
	if (Buffer.byteLength(line, 'utf8') + 1 > MAX_FRAME_BYTES) {
		throw new ProtocolError('FRAME_TOO_LARGE', '协议帧过大');
	}

	let value: unknown;
	try {
		value = JSON.parse(line);
	} catch {
		throw invalidRequest();
	}

	if (value === null || typeof value !== 'object' || Array.isArray(value)) {
		throw invalidRequest();
	}
	const frame = value as Record<string, unknown>;
	if (frame.protocolVersion !== PROTOCOL_VERSION) {
		throw new ProtocolError('PROTOCOL_MISMATCH', '协议版本不匹配');
	}
	if (
		typeof frame.id !== 'string' ||
		frame.id.length === 0 ||
		typeof frame.command !== 'string' ||
		frame.command.length === 0 ||
		frame.params === null ||
		typeof frame.params !== 'object' ||
		Array.isArray(frame.params)
	) {
		throw invalidRequest();
	}

	return {
		id: frame.id,
		protocolVersion: PROTOCOL_VERSION,
		command: frame.command,
		params: frame.params as Record<string, unknown>,
	};
}

export function successFrame(id: string, result: unknown): SuccessFrame {
	return { id, ok: true, result };
}

export function failureFrame(id: string, code: string, message: string): FailureFrame {
	return { id, ok: false, error: { code, message } };
}
