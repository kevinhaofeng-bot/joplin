import Folder from '../../../lib/models/Folder';
import ItemChange from '../../../lib/models/ItemChange';
import type { FolderEntity } from '../../../lib/services/database/types';
import { ProtocolError } from '../protocol';
import { folderDto, type FolderDto } from './dto';
import { conflictError, normalizeFolderTitle, notFoundError, storageError, validateId, validateTimestamp, validationError } from './validation';

export type CreateFolderInput = { id?: unknown; parentId: unknown; title: unknown };
export type UpdateFolderInput = { id: unknown; expectedUpdatedTime: unknown; title?: unknown; parentId?: unknown };
export type FolderMutation = { item: FolderDto; created?: boolean; changed?: boolean };

type FolderModel = typeof Folder;
type StoreOptions = { model?: FolderModel; waitForChanges?: () => Promise<unknown>; now?: () => number };

function sameCreate(existing: FolderEntity, parentId: string, title: string): boolean {
	return !existing.deleted_time && (existing.parent_id || '') === parentId && existing.title === title;
}

function fixedStorage(error: unknown): never {
	if (error instanceof ProtocolError) throw error;
	throw storageError();
}

export class FolderStore {
	private readonly model: FolderModel;
	private readonly waitForChanges: () => Promise<unknown>;
	private readonly now: () => number;

	public constructor(options: StoreOptions = {}) {
		this.model = options.model ?? Folder;
		this.waitForChanges = options.waitForChanges ?? ItemChange.waitForAllSaved;
		this.now = options.now ?? Date.now;
	}

	public async list(): Promise<FolderDto[]> {
		try {
			return (await this.model.all({ includeDeleted: false })).map(folderDto);
		} catch (error) {
			return fixedStorage(error);
		}
	}

	public async create(input: CreateFolderInput): Promise<FolderMutation> {
		const parentId = this.parentId(input.parentId);
		const title = normalizeFolderTitle(input.title);
		const id = input.id === undefined ? undefined : validateId(input.id);
		try {
			await this.requireActiveParent(parentId);
			if (id) {
				const existing = await this.model.load(id);
				if (existing) {
					if (sameCreate(existing, parentId, title)) return { item: folderDto(existing), created: false };
					throw conflictError();
				}
			}
			const saved = await this.model.save({ id, parent_id: parentId, title }, { isNew: true, userSideValidation: true });
			await this.waitForChanges();
			return { item: folderDto(await this.model.load(saved.id) || saved), created: true };
		} catch (error) {
			return fixedStorage(error);
		}
	}

	public async update(input: UpdateFolderInput): Promise<FolderMutation> {
		const id = validateId(input.id);
		const expectedUpdatedTime = validateTimestamp(input.expectedUpdatedTime);
		if (!Object.prototype.hasOwnProperty.call(input, 'title') && !Object.prototype.hasOwnProperty.call(input, 'parentId')) throw validationError();
		const title = input.title === undefined ? undefined : normalizeFolderTitle(input.title);
		const parentId = input.parentId === undefined ? undefined : this.parentId(input.parentId);
		try {
			const current = await this.model.load(id);
			if (!current || current.deleted_time) throw notFoundError();
			if (current.updated_time !== expectedUpdatedTime) throw conflictError();
			if (parentId !== undefined) {
				await this.requireActiveParent(parentId);
				if (!(await this.model.canNestUnder(id, parentId))) throw validationError();
			}
			const nextTitle = title === undefined ? current.title : title;
			const nextParent = parentId === undefined ? (current.parent_id || '') : parentId;
			if (nextTitle === current.title && nextParent === (current.parent_id || '')) return { item: folderDto(current), changed: false };
			const updatedTime = Math.max(this.now(), (current.updated_time || 0) + 1);
			const saved = await this.model.save({
				id, title: nextTitle, parent_id: nextParent, updated_time: updatedTime,
				...(title === undefined ? {} : { user_updated_time: updatedTime }),
			}, { isNew: false, autoTimestamp: false, userSideValidation: true });
			await this.waitForChanges();
			return { item: folderDto(await this.model.load(id) || saved), changed: true };
		} catch (error) {
			return fixedStorage(error);
		}
	}

	public async trash(idValue: unknown, expectedUpdatedTimeValue: unknown): Promise<{ id: string; deletedTime: number }> {
		const id = validateId(idValue);
		const expectedUpdatedTime = validateTimestamp(expectedUpdatedTimeValue);
		try {
			const current = await this.model.load(id);
			if (!current || current.deleted_time) throw notFoundError();
			if (current.updated_time !== expectedUpdatedTime) throw conflictError();
			await this.model.delete(id, { toTrash: true, deleteChildren: true, sourceDescription: 'app-lite/trashFolder' });
			await this.waitForChanges();
			const trashed = await this.model.load(id);
			if (!trashed) throw storageError();
			return { id, deletedTime: trashed.deleted_time || 0 };
		} catch (error) {
			return fixedStorage(error);
		}
	}

	private parentId(value: unknown): string {
		if (value === '') return '';
		return validateId(value);
	}

	private async requireActiveParent(parentId: string): Promise<void> {
		if (!parentId) return;
		const parent = await this.model.load(parentId);
		if (!parent || parent.deleted_time) throw notFoundError();
	}
}
