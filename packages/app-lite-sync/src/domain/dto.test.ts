import { folderDto, tagDto } from './dto';

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
});
