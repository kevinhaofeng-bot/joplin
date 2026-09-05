import Tag from '../../../lib/models/Tag';
import ItemChange from '../../../lib/models/ItemChange';
import { ProtocolError } from '../protocol';
import { tagDto, type TagDto } from './dto';
import { conflictError, normalizeTagTitle, notFoundError, storageError, validateId, validateTimestamp } from './validation';

export type CreateTagInput = { id?: unknown; title: unknown };
export type UpdateTagInput = { id: unknown; expectedUpdatedTime: unknown; title: unknown };
export type TagMutation = { item: TagDto; created?: boolean; changed?: boolean };

type TagModel = typeof Tag;
type StoreOptions = { model?: TagModel; waitForChanges?: () => Promise<unknown>; now?: () => number };

function fixedStorage(error: unknown): never {
	if (error instanceof ProtocolError) throw error;
	throw storageError();
}

export class TagStore {
	private readonly model: TagModel;
	private readonly waitForChanges: () => Promise<unknown>;
	private readonly now: () => number;

	public constructor(options: StoreOptions = {}) {
		this.model = options.model ?? Tag;
		this.waitForChanges = options.waitForChanges ?? ItemChange.waitForAllSaved;
		this.now = options.now ?? Date.now;
	}

	public async list(): Promise<TagDto[]> {
		try {
			const [tags, withCounts] = await Promise.all([this.model.all(), this.model.allWithNotes()]);
			const counts = new Map(withCounts.map(tag => [tag.id, Number(tag.note_count || 0)]));
			return tags.map(tag => tagDto(tag, counts.get(tag.id) || 0));
		} catch (error) {
			return fixedStorage(error);
		}
	}

	public async create(input: CreateTagInput): Promise<TagMutation> {
		const title = normalizeTagTitle(input.title);
		const id = input.id === undefined ? undefined : validateId(input.id);
		try {
			const sameTitle = await this.model.loadByTitle(title);
			if (sameTitle && (!id || sameTitle.id !== id)) throw conflictError();
			if (id) {
				const existing = await this.model.load(id);
				if (existing) {
					if (existing.title === title) return { item: tagDto(existing, await this.count(id)), created: false };
					throw conflictError();
				}
			}
			const saved = await this.model.save({ id, title }, { isNew: true, userSideValidation: true });
			await this.waitForChanges();
			return { item: tagDto(await this.model.load(saved.id) || saved, await this.count(saved.id)), created: true };
		} catch (error) {
			return fixedStorage(error);
		}
	}

	public async update(input: UpdateTagInput): Promise<TagMutation> {
		const id = validateId(input.id);
		const expectedUpdatedTime = validateTimestamp(input.expectedUpdatedTime);
		const title = normalizeTagTitle(input.title);
		try {
			const current = await this.model.load(id);
			if (!current) throw notFoundError();
			if (current.updated_time !== expectedUpdatedTime) throw conflictError();
			const sameTitle = await this.model.loadByTitle(title);
			if (sameTitle && sameTitle.id !== id) throw conflictError();
			if (current.title === title) return { item: tagDto(current, await this.count(id)), changed: false };
			const updatedTime = Math.max(this.now(), (current.updated_time || 0) + 1);
			const saved = await this.model.save({ id, title, updated_time: updatedTime, user_updated_time: updatedTime }, { isNew: false, autoTimestamp: false, userSideValidation: true });
			await this.waitForChanges();
			return { item: tagDto(await this.model.load(id) || saved, await this.count(id)), changed: true };
		} catch (error) {
			return fixedStorage(error);
		}
	}

	public async delete(idValue: unknown, expectedUpdatedTimeValue: unknown): Promise<{ id: string; deleted: true }> {
		const id = validateId(idValue);
		const expectedUpdatedTime = validateTimestamp(expectedUpdatedTimeValue);
		try {
			const current = await this.model.load(id);
			if (!current) throw notFoundError();
			if (current.updated_time !== expectedUpdatedTime) throw conflictError();
			await this.model.untagAll(id);
			await this.waitForChanges();
			return { id, deleted: true };
		} catch (error) {
			return fixedStorage(error);
		}
	}

	private async count(id: string): Promise<number> {
		const all = await this.model.allWithNotes();
		const found = all.find(tag => tag.id === id);
		return Number(found?.note_count || 0);
	}
}
