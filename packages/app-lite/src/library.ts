import { invoke } from '@tauri-apps/api/core';

export type MarkupLanguage = 'markdown' | 'html';
export type ProfileState = 'closed' | 'open';

export interface ProfileStatus { state: ProfileState; formatVersion: number }
export interface OpenProfile { state: 'open'; schemaVersion: number; formatVersion: number }
export interface Folder { id: string; parentId: string; title: string; createdTime: number; updatedTime: number; deletedTime: number }
export interface Tag { id: string; title: string; createdTime: number; updatedTime: number; noteCount: number }
export interface NoteSummary {
	id: string; parentId: string; title: string; isTodo: boolean; todoDue: number; todoCompleted: number;
	createdTime: number; updatedTime: number; userCreatedTime: number; userUpdatedTime: number; deletedTime: number;
}
export interface NoteDetail extends NoteSummary { body: string; markupLanguage: MarkupLanguage; tagIds: string[] }
export interface NotePage { items: NoteSummary[]; page: number; hasMore: boolean }

export interface CreateFolderParams { id?: string; parentId: string; title: string }
export interface UpdateFolderParams { id: string; expectedUpdatedTime: number; title?: string; parentId?: string }
export interface CreateTagParams { id?: string; title: string }
export interface UpdateTagParams { id: string; expectedUpdatedTime: number; title: string }
export interface ListNotesParams { parentId?: string; page?: number; limit?: number }
export interface GetByIdParams { id: string }
export interface ExpectedUpdatedTimeParams { id: string; expectedUpdatedTime: number }
export interface CreateNoteParams { id?: string; parentId: string; title: string; body: string; isTodo?: boolean; todoDue?: number }
export interface UpdateNoteParams {
	id: string; expectedUpdatedTime: number; title?: string; body?: string; parentId?: string;
	isTodo?: boolean; todoDue?: number; todoCompleted?: number;
}
export interface SetNoteTagsParams { noteId: string; expectedUpdatedTime: number; tagIds: string[] }

export interface CreateResult<T> { item: T; created: boolean }
export interface UpdateResult<T> { item: T; changed: boolean }
export interface TrashResult { id: string; deletedTime: number }
export interface DeleteResult { id: string; deleted: boolean }
export interface SetNoteTagsResult { noteId: string; tagIds: string[]; updatedTime: number; changed: boolean }

const stableMessages: Record<string, string> = {
	SIDECAR_UNAVAILABLE: '本地资料库不可用', PROFILE_IN_USE: '资料库正在被使用', PROFILE_LOCK_REQUIRED: '资料库写入租约无效',
	PROFILE_INVALID: '资料库路径无效', PROFILE_NOT_OWNED: '资料库不属于 Joplin Lite', PROFILE_ALREADY_OPEN: '资料库已经打开',
	PROFILE_NOT_OPEN: '资料库尚未打开', PROFILE_OPEN_FAILED: '无法打开资料库', STORAGE_ERROR: '无法保存资料库',
	NOT_FOUND: '项目不存在', VALIDATION_FAILED: '输入内容无效', CONFLICT: '项目已被其他操作修改',
	SIDECAR_FAILED: '本地资料库操作失败',
};

export class LibraryClientError extends Error {
	readonly code: string;
	constructor(code: string, message?: string) {
		const safeCode = code in stableMessages ? code : 'SIDECAR_FAILED';
		super(stableMessages[safeCode] ?? message ?? stableMessages.SIDECAR_FAILED);
		this.name = 'LibraryClientError';
		this.code = safeCode;
	}
}

export function normalizeLibraryError(error: unknown): LibraryClientError {
	if (error instanceof LibraryClientError) return error;
	if (isRecord(error) && typeof error.code === 'string') return new LibraryClientError(error.code);
	return new LibraryClientError('SIDECAR_FAILED');
}

const INVALID_LIBRARY_RESPONSE = '本地资料库响应无效';
const isRecord = (value: unknown): value is Record<string, unknown> => value !== null && typeof value === 'object' && !Array.isArray(value);
const isId = (value: unknown): value is string => typeof value === 'string' && /^[0-9a-f]{32}$/.test(value);
const isTime = (value: unknown): value is number => typeof value === 'number' && Number.isSafeInteger(value) && value >= 0;
const isPositiveTime = (value: unknown): value is number => isTime(value) && value > 0;
const isState = (value: unknown): value is ProfileState => value === 'closed' || value === 'open';
const invalid = (): never => { throw new Error(INVALID_LIBRARY_RESPONSE); };

function guardProfileStatus(value: unknown): ProfileStatus {
	if (!isRecord(value) || !isState(value.state) || !isTime(value.formatVersion)) return invalid();
	return value as unknown as ProfileStatus;
}
function guardOpenProfile(value: unknown): OpenProfile {
	if (!isRecord(value) || value.state !== 'open' || !isPositiveTime(value.schemaVersion) || !isPositiveTime(value.formatVersion)) return invalid();
	return value as unknown as OpenProfile;
}
function guardFolder(value: unknown): Folder {
	if (!isRecord(value) || !isId(value.id) || (value.parentId !== '' && !isId(value.parentId)) || typeof value.title !== 'string' ||
		!isTime(value.createdTime) || !isTime(value.updatedTime) || !isTime(value.deletedTime)) return invalid();
	return value as unknown as Folder;
}
function guardTag(value: unknown): Tag {
	if (!isRecord(value) || !isId(value.id) || typeof value.title !== 'string' || !isTime(value.createdTime) ||
		!isTime(value.updatedTime) || !isTime(value.noteCount)) return invalid();
	return value as unknown as Tag;
}
function guardNoteSummary(value: unknown): NoteSummary {
	if (!isRecord(value) || !isId(value.id) || !isId(value.parentId) || typeof value.title !== 'string' ||
		typeof value.isTodo !== 'boolean' || !isTime(value.todoDue) || !isTime(value.todoCompleted) ||
		!isTime(value.createdTime) || !isTime(value.updatedTime) || !isTime(value.userCreatedTime) ||
		!isTime(value.userUpdatedTime) || !isTime(value.deletedTime)) return invalid();
	return value as unknown as NoteSummary;
}
function guardNote(value: unknown): NoteDetail {
	if (!isRecord(value) || typeof value.body !== 'string' || (value.markupLanguage !== 'markdown' && value.markupLanguage !== 'html') ||
		!Array.isArray(value.tagIds) || !value.tagIds.every(isId)) return invalid();
	guardNoteSummary(value);
	return value as unknown as NoteDetail;
}
function guardArray<T>(value: unknown, guard: (item: unknown)=> T): T[] {
	if (!Array.isArray(value)) return invalid();
	return value.map(guard);
}
function guardCreate<T>(value: unknown, guard: (item: unknown)=> T): CreateResult<T> {
	if (!isRecord(value) || typeof value.created !== 'boolean') return invalid();
	return { created: value.created, item: guard(value.item) };
}
function guardUpdate<T>(value: unknown, guard: (item: unknown)=> T): UpdateResult<T> {
	if (!isRecord(value) || typeof value.changed !== 'boolean') return invalid();
	return { changed: value.changed, item: guard(value.item) };
}
function guardTrash(value: unknown): TrashResult {
	if (!isRecord(value) || !isId(value.id) || !isTime(value.deletedTime)) return invalid();
	return value as unknown as TrashResult;
}
function guardDelete(value: unknown): DeleteResult {
	if (!isRecord(value) || !isId(value.id) || value.deleted !== true) return invalid();
	return value as unknown as DeleteResult;
}
function guardSetTags(value: unknown): SetNoteTagsResult {
	if (!isRecord(value) || !isId(value.noteId) || !Array.isArray(value.tagIds) || !value.tagIds.every(isId) ||
		!isTime(value.updatedTime) || typeof value.changed !== 'boolean') return invalid();
	return value as unknown as SetNoteTagsResult;
}
function guardPage(value: unknown): NotePage {
	if (!isRecord(value) || !isPositiveTime(value.page) || typeof value.hasMore !== 'boolean') return invalid();
	return { items: guardArray(value.items, guardNoteSummary), page: value.page, hasMore: value.hasMore };
}
async function call<T>(command: string, guard: (value: unknown)=> T, params?: unknown): Promise<T> {
	try {
		const value = params === undefined ? await invoke<unknown>(command) : await invoke<unknown>(command, { params });
		return guard(value);
	} catch (error) {
		if (error instanceof Error && error.message === INVALID_LIBRARY_RESPONSE) throw error;
		throw normalizeLibraryError(error);
	}
}

export const profileStatus = () => call('profile_status', guardProfileStatus);
export const openLibrary = () => call('open_library', guardOpenProfile);
export const retryLibrary = () => call('retry_library', guardOpenProfile);
export const shutdownLibrary = () => call('shutdown_library', value => {
	if (value !== null && value !== undefined) return invalid();
	return undefined;
});
export const listFolders = () => call('list_folders', value => guardArray(value, guardFolder));
export const createFolder = (params: CreateFolderParams) => call('create_folder', value => guardCreate(value, guardFolder), params);
export const updateFolder = (params: UpdateFolderParams) => call('update_folder', value => guardUpdate(value, guardFolder), params);
export const trashFolder = (params: ExpectedUpdatedTimeParams) => call('trash_folder', guardTrash, params);
export const listTags = () => call('list_tags', value => guardArray(value, guardTag));
export const createTag = (params: CreateTagParams) => call('create_tag', value => guardCreate(value, guardTag), params);
export const updateTag = (params: UpdateTagParams) => call('update_tag', value => guardUpdate(value, guardTag), params);
export const deleteTag = (params: ExpectedUpdatedTimeParams) => call('delete_tag', guardDelete, params);
export const listNotes = (params: ListNotesParams = {}) => call('list_notes', guardPage, params);
export const getNote = (params: GetByIdParams) => call('get_note', guardNote, params);
export const createNote = (params: CreateNoteParams) => call('create_note', value => guardCreate(value, guardNote), params);
export const updateNote = (params: UpdateNoteParams) => call('update_note', value => guardUpdate(value, guardNote), params);
export const trashNote = (params: ExpectedUpdatedTimeParams) => call('trash_note', guardTrash, params);
export const setNoteTags = (params: SetNoteTagsParams) => call('set_note_tags', guardSetTags, params);

export { INVALID_LIBRARY_RESPONSE };

export interface LibraryApi {
	openLibrary: typeof openLibrary; retryLibrary: typeof retryLibrary; shutdownLibrary: typeof shutdownLibrary;
	profileStatus: typeof profileStatus; listFolders: typeof listFolders; createFolder: typeof createFolder;
	updateFolder: typeof updateFolder; trashFolder: typeof trashFolder; listTags: typeof listTags;
	createTag: typeof createTag; updateTag: typeof updateTag; deleteTag: typeof deleteTag;
	listNotes: typeof listNotes; getNote: typeof getNote; createNote: typeof createNote;
	updateNote: typeof updateNote; trashNote: typeof trashNote; setNoteTags: typeof setNoteTags;
}

export const libraryApi: LibraryApi = {
	openLibrary, retryLibrary, shutdownLibrary, profileStatus, listFolders, createFolder, updateFolder, trashFolder,
	listTags, createTag, updateTag, deleteTag, listNotes, getNote, createNote, updateNote, trashNote, setNoteTags,
};
