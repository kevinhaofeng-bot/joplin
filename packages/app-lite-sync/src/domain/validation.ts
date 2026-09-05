import { profileError, ProtocolError } from '../protocol';

export function validationError(): ProtocolError {
	return profileError('VALIDATION_FAILED');
}

export function notFoundError(): ProtocolError {
	return profileError('NOT_FOUND');
}

export function conflictError(): ProtocolError {
	return profileError('CONFLICT');
}

export function storageError(): ProtocolError {
	return profileError('STORAGE_ERROR');
}

export function validateId(value: unknown): string {
	if (typeof value !== 'string' || !/^[a-f0-9]{32}$/.test(value)) throw validationError();
	return value;
}

export function validateTimestamp(value: unknown): number {
	if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < 0) throw validationError();
	return value;
}

export function normalizeFolderTitle(value: unknown): string {
	if (typeof value !== 'string') throw validationError();
	let title = value;
	while (title.startsWith('/') || title.startsWith('\\')) title = title.slice(1);
	if (!title) throw validationError();
	return title;
}

export function normalizeTagTitle(value: unknown): string {
	if (typeof value !== 'string') throw validationError();
	const title = value.trim().normalize('NFC');
	if (!title) throw validationError();
	return title;
}

export function isObject(value: unknown): value is Record<string, unknown> {
	return value !== null && typeof value === 'object' && !Array.isArray(value);
}

export function exactKeys(value: Record<string, unknown>, keys: readonly string[]): boolean {
	return Object.keys(value).length === keys.length && keys.every(key => Object.prototype.hasOwnProperty.call(value, key));
}
