import { NoteStore } from './noteStore';

const idA = 'a'.repeat(32);
const idB = 'b'.repeat(32);
const idC = 'c'.repeat(32);

const note = (id: string, updatedTime: number, extra: Record<string, unknown> = {}) => ({
	id, parent_id: '', title: id, body: `body-${id}`, is_todo: 0, todo_due: 0, todo_completed: 0,
	created_time: 1, updated_time: updatedTime, user_created_time: 1, user_updated_time: 1,
	deleted_time: 0, is_conflict: 0, markup_language: 1, ...extra,
});

describe('note store', () => {
	test('uses official preview ordering and paginates without exposing body', async () => {
		const previews = jest.fn(async (_parentId: string|null, options: any) => {
			expect(options.fields).not.toContain('body');
			expect(options.order).toEqual([{ by: 'updated_time', dir: 'DESC' }, { by: 'id', dir: 'ASC' }]);
			return [note(idB, 2), note(idA, 2), note(idC, 1)];
		});
		const store = new NoteStore({ model: { previews } as any });
		const result = await store.list({ page: 1, limit: 2 });
		expect(result.items.map(item => item.id)).toEqual([idA, idB]);
		expect(result).toMatchObject({ page: 1, hasMore: true });
		expect(previews).toHaveBeenCalledWith(null, expect.objectContaining({ limit: 3 }));
	});

	test('replays an identical explicit create and rejects stale update before save', async () => {
		const notes = new Map([[idA, note(idA, 4, { parent_id: idB })]]) as Map<string, any>;
		const save = jest.fn(async (item: any) => { const saved = { ...notes.get(item.id), ...item }; notes.set(item.id, saved); return saved; });
		const model = { load: async (id: string) => notes.get(id) || null, save, previews: async (): Promise<any[]> => [], delete: async (): Promise<void> => undefined };
		const barrier = { begin: jest.fn(async (): Promise<number> => 0), end: jest.fn(async (): Promise<void> => undefined) };
		const store = new NoteStore({ model: model as any, folderModel: { load: async (): Promise<any> => ({ id: idB, deleted_time: 0 }) } as any, tagModel: { tagsByNoteId: async (): Promise<any[]> => [], byIds: async (): Promise<any[]> => [], setNoteTagsByIds: async (): Promise<void> => undefined } as any, barrier: barrier as any, now: () => 10 });
		await expect(store.create({ id: idA, parentId: idB, title: idA, body: `body-${idA}` })).resolves.toMatchObject({ created: false });
		await expect(store.update({ id: idA, expectedUpdatedTime: 3, body: 'new' })).rejects.toMatchObject({ code: 'CONFLICT' });
		expect(save).not.toHaveBeenCalled();
	});

	test('prevalidates every tag before setNoteTags so missing tags do not partially write', async () => {
		const current = note(idA, 4);
		const setNoteTagsByIds = jest.fn(async (): Promise<void> => undefined);
		const save = jest.fn(async () => current);
		const model = { load: async () => current, save, previews: async (): Promise<any[]> => [] };
		const tagModel = { byIds: jest.fn(async (): Promise<any[]> => [{ id: idB }]), tagsByNoteId: jest.fn(async (): Promise<any[]> => []), setNoteTagsByIds };
		const store = new NoteStore({ model: model as any, tagModel: tagModel as any, barrier: { begin: async (): Promise<number> => 0, end: async (): Promise<void> => undefined } as any });
		await expect(store.setNoteTags({ noteId: idA, expectedUpdatedTime: 4, tagIds: [idB, idC] })).rejects.toMatchObject({ code: 'NOT_FOUND' });
		expect(setNoteTagsByIds).not.toHaveBeenCalled();
		expect(save).not.toHaveBeenCalled();
	});

	test('requires an explicit non-empty active parent and rejects unsafe title input', async () => {
		const model = { load: async (): Promise<any> => null, previews: async (): Promise<any[]> => [], save: async (): Promise<any> => null, delete: async (): Promise<void> => undefined };
		const store = new NoteStore({ model: model as any, folderModel: { load: async (): Promise<any> => null } as any });
		await expect(store.create({ parentId: '', title: 'Title', body: 'Body' })).rejects.toMatchObject({ code: 'VALIDATION_FAILED' });
		await expect(store.create({ parentId: idA, title: `bad${String.fromCharCode(0)}title`, body: 'Body' })).rejects.toMatchObject({ code: 'VALIDATION_FAILED' });
		await expect(store.create({ parentId: idA, title: 'x'.repeat(4097), body: 'Body' })).rejects.toMatchObject({ code: 'VALIDATION_FAILED' });
	});

	test('does not replay an explicit note when its markup language differs', async () => {
		const existing = note(idA, 4, { parent_id: idB, markup_language: 2 });
		const model = { load: async (): Promise<any> => existing, previews: async (): Promise<any[]> => [], save: jest.fn(), delete: async (): Promise<void> => undefined };
		const store = new NoteStore({ model: model as any, folderModel: { load: async (): Promise<any> => ({ id: idB, deleted_time: 0 }) } as any, tagModel: { tagsByNoteId: async (): Promise<any[]> => [], byIds: async (): Promise<any[]> => [], setNoteTagsByIds: async (): Promise<void> => undefined } as any });
		await expect(store.create({ id: idA, parentId: idB, title: idA, body: `body-${idA}` })).rejects.toMatchObject({ code: 'CONFLICT' });
	});

	test('refreshes official note-resource associations after saving note body', async () => {
		const notes = new Map<string, any>();
		const savedBody = `![image](:/${idC})`;
		const association = jest.fn(async (): Promise<void> => undefined);
		const model = {
			load: async (id: string) => notes.get(id) || null,
			save: async (item: any) => { const result = { created_time: 1, updated_time: 1, deleted_time: 0, ...item }; notes.set(result.id, result); return result; },
			previews: async (): Promise<any[]> => [], delete: async (): Promise<void> => undefined,
		};
		const store = new NoteStore({
			model: model as any,
			folderModel: { load: async (): Promise<any> => ({ id: idB, deleted_time: 0 }) } as any,
			tagModel: { tagsByNoteId: async (): Promise<any[]> => [], byIds: async (): Promise<any[]> => [], setNoteTagsByIds: async (): Promise<void> => undefined } as any,
			barrier: { begin: async (): Promise<number> => 0, end: async (): Promise<void> => undefined } as any,
			setAssociatedResources: association,
		});

		await store.create({ id: idA, parentId: idB, title: 'with image', body: savedBody });
		expect(association).toHaveBeenCalledWith(idA, savedBody);
	});
});
