import { folderDto, noteDetailDto, noteSummaryDto, tagDto } from './dto';

describe('domain DTOs', () => {
	test('projects a folder without leaking model fields', () => {
		expect(folderDto({
			id: 'a'.repeat(32), parent_id: '', title: 'Root', created_time: 1, updated_time: 2, deleted_time: 0,
			user_updated_time: 3, encryption_cipher_text: 'secret', type_: 2,
		})).toEqual({ id: 'a'.repeat(32), parentId: '', title: 'Root', createdTime: 1, updatedTime: 2, deletedTime: 0 });
	});

	test('projects a tag with an explicit count including zero', () => {
		expect(tagDto({ id: 'b'.repeat(32), title: 'Tag', created_time: 4, updated_time: 5 }, 0)).toEqual({
			id: 'b'.repeat(32), title: 'Tag', createdTime: 4, updatedTime: 5, noteCount: 0,
		});
	});

	test('projects note summary without body and detail with sorted tags', () => {
		const note = {
			id: 'c'.repeat(32), parent_id: '', title: 'Note', body: 'secret body', is_todo: 1,
			todo_due: 4, todo_completed: 0, created_time: 5, updated_time: 6, user_created_time: 7,
			user_updated_time: 8, deleted_time: 0, markup_language: 2, encryption_cipher_text: 'secret', type_: 1,
		};
		expect(noteSummaryDto(note)).toEqual({
			id: 'c'.repeat(32), parentId: '', title: 'Note', isTodo: true, todoDue: 4, todoCompleted: 0,
			createdTime: 5, updatedTime: 6, userCreatedTime: 7, userUpdatedTime: 8, deletedTime: 0,
		});
		expect(noteDetailDto(note, ['b'.repeat(32), 'a'.repeat(32)])).toEqual({
			id: 'c'.repeat(32), parentId: '', title: 'Note', isTodo: true, todoDue: 4, todoCompleted: 0,
			createdTime: 5, updatedTime: 6, userCreatedTime: 7, userUpdatedTime: 8, deletedTime: 0,
			body: 'secret body', markupLanguage: 'html', tagIds: ['a'.repeat(32), 'b'.repeat(32)],
		});
	});
});
