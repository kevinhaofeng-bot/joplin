import { FolderStore } from './folderStore';
import { TagStore } from './tagStore';

const idA = 'a'.repeat(32);
const idB = 'b'.repeat(32);

describe('domain stores', () => {
	test('folder create is deterministic and stale update is a conflict', async () => {
		const folders = new Map<string, any>();
		const model = {
			all: async () => [...folders.values()],
			load: async (id: string) => folders.get(id) || null,
			canNestUnder: async () => true,
			save: async (item: any) => { const result = { created_time: 1, updated_time: 1, deleted_time: 0, ...folders.get(item.id), ...item }; folders.set(result.id, result); return result; },
			delete: async (): Promise<void> => undefined,
		};
		const store = new FolderStore({ model: model as any, waitForChanges: async () => undefined, now: () => 10 });

		await expect(store.create({ id: idA, parentId: '', title: '///Root' })).resolves.toMatchObject({ created: true, item: { title: 'Root' } });
		await expect(store.create({ id: idA, parentId: '', title: 'Root' })).resolves.toMatchObject({ created: false });
		await expect(store.update({ id: idA, expectedUpdatedTime: 0, title: 'Renamed' })).rejects.toMatchObject({ code: 'CONFLICT' });
	});

	test('tag list retains zero-note tags and delete uses official untagAll', async () => {
		const tags = new Map<string, any>([[idB, { id: idB, title: 'Tag', created_time: 1, updated_time: 2 }]]);
		let untagged = '';
		const model = {
			all: async () => [...tags.values()],
			allWithNotes: async (): Promise<any[]> => [],
			load: async (id: string) => tags.get(id) || null,
			loadByTitle: async (title: string) => [...tags.values()].find(tag => tag.title.toLowerCase() === title.toLowerCase()) || null,
			save: async (item: any) => { const result = { created_time: 1, updated_time: 2, ...tags.get(item.id), ...item }; tags.set(result.id, result); return result; },
			untagAll: async (id: string) => { untagged = id; tags.delete(id); },
		};
		const store = new TagStore({ model: model as any, waitForChanges: async () => undefined, now: () => 10 });

		await expect(store.list()).resolves.toEqual([{ id: idB, title: 'Tag', createdTime: 1, updatedTime: 2, noteCount: 0 }]);
		await expect(store.delete(idB, 2)).resolves.toEqual({ id: idB, deleted: true });
		expect(untagged).toBe(idB);
	});
});
