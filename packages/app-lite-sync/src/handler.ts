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
import { FolderStore, type CreateFolderInput, type UpdateFolderInput } from './domain/folderStore';
import { TagStore } from './domain/tagStore';
import type { CreateTagInput, UpdateTagInput } from './domain/tagStore';
import { NoteStore, type CreateNoteInput, type SetNoteTagsInput, type UpdateNoteInput } from './domain/noteStore';
import { exactKeys, validationError } from './domain/validation';

const joplinVersion: string = require('../../lib/package.json').version;
const capabilities = ['decodeItem', 'encodeItem', 'shutdown', 'profileStatus', 'openProfile', 'listFolders', 'createFolder', 'updateFolder', 'trashFolder', 'listTags', 'createTag', 'updateTag', 'deleteTag', 'listNotes', 'getNote', 'createNote', 'updateNote', 'trashNote', 'setNoteTags'] as const;
const terminalCodes = new Set(['PROFILE_LOCK_REQUIRED', 'PROFILE_OPEN_FAILED', 'STORAGE_ERROR']);
const stableCodes = new Set(['INVALID_ITEM', 'INVALID_REQUEST', ...Object.keys(PROFILE_ERROR_MESSAGES)]);

type HandledRequest = { response: ResponseFrame; shouldExit: boolean };
export type RequestHandler = { handleRequest(request: RequestFrame): Promise<HandledRequest> };
type SessionLike = Pick<ProfileSession, 'status' | 'open' | 'flush' | 'close' | 'requireOpen'>;

function invalidRequest(): ProtocolError {
	return new ProtocolError('INVALID_REQUEST', '请求格式无效');
}

function isObject(value: unknown): value is Record<string, unknown> {
	return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function hasOnly(params: Record<string, unknown>, keys: readonly string[]): boolean {
	return exactKeys(params, keys);
}

function hasAllowed(params: Record<string, unknown>, required: readonly string[], optional: readonly string[] = []): boolean {
	const allowed = new Set([...required, ...optional]);
	return required.every(key => Object.prototype.hasOwnProperty.call(params, key)) && Object.keys(params).every(key => allowed.has(key));
}

function requireOpen(session: SessionLike): void {
	if (session.requireOpen) return session.requireOpen();
	if (session.status().state !== 'open') throw profileError('PROFILE_NOT_OPEN');
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
	const folderStore = new FolderStore();
	const tagStore = new TagStore();
	const noteStore = new NoteStore();
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
				case 'listFolders':
					if (!isObject(request.params) || !hasOnly(request.params, [])) throw validationError();
					requireOpen(session);
					return { response: successFrame(request.id, await folderStore.list()), shouldExit: false };
				case 'createFolder':
					if (!isObject(request.params) || !hasAllowed(request.params, ['parentId', 'title'], ['id'])) throw validationError();
					requireOpen(session);
					return { response: successFrame(request.id, await folderStore.create(request.params as CreateFolderInput)), shouldExit: false };
				case 'updateFolder':
					if (!isObject(request.params) || !hasAllowed(request.params, ['id', 'expectedUpdatedTime'], ['title', 'parentId'])) throw validationError();
					requireOpen(session);
					return { response: successFrame(request.id, await folderStore.update(request.params as UpdateFolderInput)), shouldExit: false };
				case 'trashFolder':
					if (!isObject(request.params) || !hasOnly(request.params, ['id', 'expectedUpdatedTime'])) throw validationError();
					requireOpen(session);
					return { response: successFrame(request.id, await folderStore.trash(request.params.id, request.params.expectedUpdatedTime)), shouldExit: false };
				case 'listTags':
					if (!isObject(request.params) || !hasOnly(request.params, [])) throw validationError();
					requireOpen(session);
					return { response: successFrame(request.id, await tagStore.list()), shouldExit: false };
				case 'createTag':
					if (!isObject(request.params) || !hasAllowed(request.params, ['title'], ['id'])) throw validationError();
					requireOpen(session);
					return { response: successFrame(request.id, await tagStore.create(request.params as CreateTagInput)), shouldExit: false };
				case 'updateTag':
					if (!isObject(request.params) || !hasOnly(request.params, ['id', 'expectedUpdatedTime', 'title'])) throw validationError();
					requireOpen(session);
					return { response: successFrame(request.id, await tagStore.update(request.params as UpdateTagInput)), shouldExit: false };
				case 'deleteTag':
					if (!isObject(request.params) || !hasOnly(request.params, ['id', 'expectedUpdatedTime'])) throw validationError();
					requireOpen(session);
					return { response: successFrame(request.id, await tagStore.delete(request.params.id, request.params.expectedUpdatedTime)), shouldExit: false };
				case 'listNotes':
					if (!isObject(request.params) || !hasAllowed(request.params, [], ['parentId', 'page', 'limit'])) throw validationError();
					requireOpen(session);
					return { response: successFrame(request.id, await noteStore.list(request.params)), shouldExit: false };
				case 'getNote':
					if (!isObject(request.params) || !hasOnly(request.params, ['id'])) throw validationError();
					requireOpen(session);
					return { response: successFrame(request.id, await noteStore.get(request.params.id)), shouldExit: false };
				case 'createNote':
					if (!isObject(request.params) || !hasAllowed(request.params, ['parentId', 'title', 'body'], ['id', 'isTodo', 'todoDue'])) throw validationError();
					requireOpen(session);
					return { response: successFrame(request.id, await noteStore.create(request.params as CreateNoteInput)), shouldExit: false };
				case 'updateNote':
					if (!isObject(request.params) || !hasAllowed(request.params, ['id', 'expectedUpdatedTime'], ['title', 'body', 'parentId', 'isTodo', 'todoDue', 'todoCompleted'])) throw validationError();
					requireOpen(session);
					return { response: successFrame(request.id, await noteStore.update(request.params as UpdateNoteInput)), shouldExit: false };
				case 'trashNote':
					if (!isObject(request.params) || !hasOnly(request.params, ['id', 'expectedUpdatedTime'])) throw validationError();
					requireOpen(session);
					return { response: successFrame(request.id, await noteStore.trash(request.params.id, request.params.expectedUpdatedTime)), shouldExit: false };
				case 'setNoteTags':
					if (!isObject(request.params) || !hasOnly(request.params, ['noteId', 'expectedUpdatedTime', 'tagIds'])) throw validationError();
					requireOpen(session);
					return { response: successFrame(request.id, await noteStore.setNoteTags(request.params as SetNoteTagsInput)), shouldExit: false };
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
