import { SearchStore } from './searchStore';
import type { NoteEntity } from '../../../lib/services/database/types';

const note = (id: string, title: string): NoteEntity => ({
	id, parent_id: 'f'.repeat(32), title, body: 'body', is_todo: 0, todo_due: 0, todo_completed: 0,
	created_time: 1, updated_time: 2, user_created_time: 1, user_updated_time: 2, deleted_time: 0, is_conflict: 0,
} as NoteEntity);

describe('SearchStore', () => {
	test.each([
		[{ query: '' }, 'empty query'],
		[{ query: 'x', limit: 0 }, 'invalid limit'],
		[{ query: 'x', limit: 101 }, 'large limit'],
	])('rejects %s', async (value, _label) => {
		const store = new SearchStore(async () => ({ notes: [], results: [] }));
		await expect(store.search(value)).rejects.toMatchObject({ code: 'VALIDATION_FAILED' });
	});

	test('maps official relevance fields without exposing note bodies', async () => {
		const store = new SearchStore(async () => ({
			notes: [note('a'.repeat(32), 'Title')],
			results: [{ id: 'a'.repeat(32), fields: ['body'] }],
		}));
		await expect(store.search({ query: 'body' })).resolves.toEqual({
			query: 'body',
			items: [{
				id: 'a'.repeat(32), parentId: 'f'.repeat(32), title: 'Title', isTodo: false, todoDue: 0, todoCompleted: 0,
				createdTime: 1, updatedTime: 2, userCreatedTime: 1, userUpdatedTime: 2, deletedTime: 0, bodyMatch: true,
			}],
		});
	});
});
