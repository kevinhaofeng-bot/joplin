import BaseModel from '../../../lib/BaseModel';
import Folder from '../../../lib/models/Folder';
import ItemChange from '../../../lib/models/ItemChange';
import Note from '../../../lib/models/Note';
import Tag from '../../../lib/models/Tag';
import ResourceService from '../../../lib/services/ResourceService';
import type { NoteEntity } from '../../../lib/services/database/types';
import { ProtocolError } from '../protocol';
import { noteDetailDto, noteSummaryDto, type NoteDetailDto, type NoteSummaryDto } from './dto';
import { conflictError, notFoundError, storageError, validateId, validateTimestamp, validationError } from './validation';

const MARKUP_LANGUAGE_MARKDOWN = 1;
const SUMMARY_FIELDS = ['id', 'parent_id', 'title', 'is_todo', 'todo_due', 'todo_completed', 'created_time', 'updated_time', 'user_created_time', 'user_updated_time', 'deleted_time', 'is_conflict', 'markup_language'];

export type ListNotesInput = { parentId?: unknown; page?: unknown; limit?: unknown };
export type CreateNoteInput = { id?: unknown; parentId: unknown; title: unknown; body: unknown; isTodo?: unknown; todoDue?: unknown };
export type UpdateNoteInput = { id: unknown; expectedUpdatedTime: unknown; title?: unknown; body?: unknown; parentId?: unknown; isTodo?: unknown; todoDue?: unknown; todoCompleted?: unknown };
export type SetNoteTagsInput = { noteId: unknown; expectedUpdatedTime: unknown; tagIds: unknown };
export type NoteMutation = { item: NoteDetailDto; created?: boolean; changed?: boolean };

type NoteModel = Pick<typeof Note, 'previews'|'load'|'save'|'delete'>;
type FolderModel = Pick<typeof Folder, 'load'>;
type TagModel = Pick<typeof Tag, 'tagsByNoteId'|'byIds'|'setNoteTagsByIds'>;
type ChangeBarrier = {
	begin: () => Promise<number>;
	end: (barrier: number, noteId: string, changeType: number) => Promise<void>;
};
type StoreOptions = {
	model?: NoteModel;
	folderModel?: FolderModel;
	tagModel?: TagModel;
	barrier?: ChangeBarrier;
	now?: () => number;
	setAssociatedResources?: (noteId: string, body: string)=> Promise<void>;
};

function fixedStorage(error: unknown): never {
	if (error instanceof ProtocolError) throw error;
	throw storageError();
}

function active(note: NoteEntity | null | undefined): note is NoteEntity {
	return !!note && !note.deleted_time && !note.is_conflict;
}

function noteFieldsEqual(note: NoteEntity, values: { parent_id: string; title: string; body: string; is_todo: number; todo_due: number; todo_completed: number; markup_language?: number }): boolean {
	return (note.parent_id || '') === values.parent_id && (note.title || '') === values.title && (note.body || '') === values.body && !!note.is_todo === !!values.is_todo && (note.todo_due || 0) === values.todo_due && (note.todo_completed || 0) === values.todo_completed && (values.markup_language === undefined || (note.markup_language || MARKUP_LANGUAGE_MARKDOWN) === values.markup_language);
}

function defaultBarrier(): ChangeBarrier {
	return {
		begin: async () => {
			await ItemChange.waitForAllSaved();
			return ItemChange.lastChangeId();
		},
		end: async (barrier, noteId, changeType) => {
			await ItemChange.waitForAllSaved();
			const changes = await ItemChange.changesSinceId(barrier, { fields: ['id', 'item_type', 'item_id', 'type'], limit: 100 });
			if (!changes.some(change => change.item_type === BaseModel.TYPE_NOTE && change.item_id === noteId && change.type === changeType)) throw storageError();
		},
	};
}

export class NoteStore {
	private readonly model: NoteModel;
	private readonly folderModel: FolderModel;
	private readonly tagModel: TagModel;
	private readonly barrier: ChangeBarrier;
	private readonly now: () => number;
	private readonly setAssociatedResources: (noteId: string, body: string)=> Promise<void>;

	public constructor(options: StoreOptions = {}) {
		this.model = options.model ?? Note;
		this.folderModel = options.folderModel ?? Folder;
		this.tagModel = options.tagModel ?? Tag;
		this.barrier = options.barrier ?? defaultBarrier();
		this.now = options.now ?? Date.now;
		this.setAssociatedResources = options.setAssociatedResources ?? ((noteId, body) => ResourceService.instance().setAssociatedResources(noteId, body));
	}

	public async list(input: ListNotesInput = {}): Promise<{ items: NoteSummaryDto[]; page: number; hasMore: boolean }> {
		const parentId = input.parentId === undefined ? undefined : this.parentId(input.parentId);
		const page = this.page(input.page);
		const limit = this.limit(input.limit);
		try {
			if (parentId !== undefined) await this.requireActiveParent(parentId);
			const start = (page - 1) * limit;
			const notes = await this.model.previews(parentId === undefined ? null : parentId, {
				fields: SUMMARY_FIELDS,
				order: [{ by: 'updated_time', dir: 'DESC' }, { by: 'id', dir: 'ASC' }],
				limit: start + limit + 1,
			});
			const sorted = notes.slice().sort((a, b) => (b.updated_time || 0) - (a.updated_time || 0) || (a.id || '').localeCompare(b.id || ''));
			const pageItems = sorted.slice(start, start + limit);
			return { items: pageItems.map(noteSummaryDto), page, hasMore: sorted.length > start + limit };
		} catch (error) {
			return fixedStorage(error);
		}
	}

	public async get(idValue: unknown): Promise<NoteDetailDto> {
		const id = validateId(idValue);
		try {
			const note = await this.model.load(id);
			if (!active(note)) throw notFoundError();
			return noteDetailDto(note, await this.tagIds(id));
		} catch (error) {
			return fixedStorage(error);
		}
	}

	public async create(input: CreateNoteInput): Promise<NoteMutation> {
		const parentId = this.parentId(input.parentId);
		const title = this.text(input.title, true);
		const body = this.text(input.body);
		const isTodo = this.boolean(input.isTodo, false);
		const todoDue = this.timestamp(input.todoDue, 0);
		const id = input.id === undefined ? undefined : validateId(input.id);
		try {
			await this.requireActiveParent(parentId);
			if (id) {
				const existing = await this.model.load(id);
				if (existing) {
					if (active(existing) && noteFieldsEqual(existing, { parent_id: parentId, title, body, is_todo: isTodo ? 1 : 0, todo_due: todoDue, todo_completed: 0, markup_language: MARKUP_LANGUAGE_MARKDOWN })) return { item: noteDetailDto(existing, await this.tagIds(id)), created: false };
					throw conflictError();
				}
			}
			const barrier = await this.barrier.begin();
			const saved = await this.model.save({ id, parent_id: parentId, title, body, is_todo: isTodo ? 1 : 0, todo_due: todoDue, todo_completed: 0, markup_language: MARKUP_LANGUAGE_MARKDOWN, deleted_time: 0 }, { isNew: true, userSideValidation: true });
			const savedId = saved.id || id;
			if (!savedId) throw storageError();
			await this.barrier.end(barrier, savedId, ItemChange.TYPE_CREATE);
			await this.setAssociatedResources(savedId, body);
			const result = await this.model.load(savedId);
			if (!result) throw storageError();
			return { item: noteDetailDto(result, await this.tagIds(savedId)), created: true };
		} catch (error) {
			return fixedStorage(error);
		}
	}

	public async update(input: UpdateNoteInput): Promise<NoteMutation> {
		const id = validateId(input.id);
		const expectedUpdatedTime = validateTimestamp(input.expectedUpdatedTime);
		const fields = ['title', 'body', 'parentId', 'isTodo', 'todoDue', 'todoCompleted'];
		if (!fields.some(field => Object.prototype.hasOwnProperty.call(input, field))) throw validationError();
		const title = input.title === undefined ? undefined : this.text(input.title, true);
		const body = input.body === undefined ? undefined : this.text(input.body);
		const parentId = input.parentId === undefined ? undefined : this.parentId(input.parentId);
		const isTodo = input.isTodo === undefined ? undefined : this.boolean(input.isTodo);
		const todoDue = input.todoDue === undefined ? undefined : this.timestamp(input.todoDue);
		const todoCompleted = input.todoCompleted === undefined ? undefined : this.timestamp(input.todoCompleted);
		try {
			const current = await this.model.load(id);
			if (!active(current)) throw notFoundError();
			if (current.updated_time !== expectedUpdatedTime) throw conflictError();
			if (parentId !== undefined) await this.requireActiveParent(parentId);
			const values = {
				parent_id: parentId === undefined ? (current.parent_id || '') : parentId,
				title: title === undefined ? (current.title || '') : title,
				body: body === undefined ? (current.body || '') : body,
				is_todo: isTodo === undefined ? (current.is_todo || 0) : (isTodo ? 1 : 0),
				todo_due: todoDue === undefined ? (current.todo_due || 0) : todoDue,
				todo_completed: todoCompleted === undefined ? (current.todo_completed || 0) : todoCompleted,
			};
			if (noteFieldsEqual(current, values)) return { item: noteDetailDto(current, await this.tagIds(id)), changed: false };
			const updatedTime = Math.max(this.now(), (current.updated_time || 0) + 1);
			const userChanged = title !== undefined || body !== undefined || isTodo !== undefined || todoDue !== undefined || todoCompleted !== undefined;
			const barrier = await this.barrier.begin();
			const saved = await this.model.save({ ...current, ...values, updated_time: updatedTime, ...(userChanged ? { user_updated_time: updatedTime } : { user_updated_time: current.user_updated_time }) }, { isNew: false, autoTimestamp: false, userSideValidation: true });
			await this.barrier.end(barrier, id, ItemChange.TYPE_UPDATE);
			if (body !== undefined) await this.setAssociatedResources(id, values.body ?? current.body ?? '');
			const result = await this.model.load(id) || saved;
			return { item: noteDetailDto(result, await this.tagIds(id)), changed: true };
		} catch (error) {
			return fixedStorage(error);
		}
	}

	public async trash(idValue: unknown, expectedUpdatedTimeValue: unknown): Promise<{ id: string; deletedTime: number }> {
		const id = validateId(idValue);
		const expectedUpdatedTime = validateTimestamp(expectedUpdatedTimeValue);
		try {
			const current = await this.model.load(id);
			if (!active(current)) throw notFoundError();
			if (current.updated_time !== expectedUpdatedTime) throw conflictError();
			const barrier = await this.barrier.begin();
			await this.model.delete(id, { toTrash: true, sourceDescription: 'app-lite/trashNote' });
			await this.barrier.end(barrier, id, ItemChange.TYPE_UPDATE);
			const trashed = await this.model.load(id);
			if (!trashed || !trashed.deleted_time) throw storageError();
			return { id, deletedTime: trashed.deleted_time };
		} catch (error) {
			return fixedStorage(error);
		}
	}

	public async setNoteTags(input: SetNoteTagsInput): Promise<{ noteId: string; tagIds: string[]; updatedTime: number; changed: boolean }> {
		const noteId = validateId(input.noteId);
		const expectedUpdatedTime = validateTimestamp(input.expectedUpdatedTime);
		if (!Array.isArray(input.tagIds)) throw validationError();
		const tagIds = [...new Set(input.tagIds.map(validateId))].sort();
		try {
			const current = await this.model.load(noteId);
			if (!active(current)) throw notFoundError();
			if (current.updated_time !== expectedUpdatedTime) throw conflictError();
			const tags = tagIds.length ? await this.tagModel.byIds(tagIds) : [];
			if (tags.length !== tagIds.length || tags.some(tag => !tag.id || !tagIds.includes(tag.id))) throw notFoundError();
			const previous = await this.tagIds(noteId);
			if (previous.length === tagIds.length && previous.every((id, index) => id === tagIds[index])) return { noteId, tagIds, updatedTime: current.updated_time || 0, changed: false };
			const updatedTime = Math.max(this.now(), (current.updated_time || 0) + 1);
			const barrier = await this.barrier.begin();
			await this.tagModel.setNoteTagsByIds(noteId, tagIds);
			await this.model.save({ ...current, updated_time: updatedTime, user_updated_time: current.user_updated_time }, { isNew: false, autoTimestamp: false, userSideValidation: true });
			await this.barrier.end(barrier, noteId, ItemChange.TYPE_UPDATE);
			return { noteId, tagIds, updatedTime, changed: true };
		} catch (error) {
			return fixedStorage(error);
		}
	}

	private async tagIds(noteId: string): Promise<string[]> {
		const tags = await this.tagModel.tagsByNoteId(noteId);
		return tags.map(tag => tag.id).filter((id): id is string => !!id).sort();
	}

	private async requireActiveParent(parentId: string): Promise<void> {
		if (!parentId) return;
		const parent = await this.folderModel.load(parentId);
		if (!parent || parent.deleted_time) throw notFoundError();
	}

	private parentId(value: unknown): string {
		if (value === '') throw validationError();
		return validateId(value);
	}

	private text(value: unknown, title = false): string {
		if (typeof value !== 'string') throw validationError();
		if (value.includes(String.fromCharCode(0)) || (title && value.length > 4096)) throw validationError();
		return value;
	}

	private boolean(value: unknown, defaultValue?: boolean): boolean {
		if (value === undefined && defaultValue !== undefined) return defaultValue;
		if (typeof value !== 'boolean') throw validationError();
		return value;
	}

	private timestamp(value: unknown, defaultValue?: number): number {
		if (value === undefined && defaultValue !== undefined) return defaultValue;
		return validateTimestamp(value);
	}

	private page(value: unknown): number {
		if (value === undefined) return 1;
		if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < 1) throw validationError();
		return value;
	}

	private limit(value: unknown): number {
		if (value === undefined) return 50;
		if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < 1 || value > 100) throw validationError();
		return value;
	}
}
