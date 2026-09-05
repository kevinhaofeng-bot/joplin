import type { FolderEntity, NoteEntity, TagEntity } from '../../../lib/services/database/types';

export type FolderDto = {
	id: string;
	parentId: string;
	title: string;
	createdTime: number;
	updatedTime: number;
	deletedTime: number;
};

export type TagDto = {
	id: string;
	title: string;
	createdTime: number;
	updatedTime: number;
	noteCount: number;
};

export type NoteSummaryDto = {
	id: string;
	parentId: string;
	title: string;
	isTodo: boolean;
	todoDue: number;
	todoCompleted: number;
	createdTime: number;
	updatedTime: number;
	userCreatedTime: number;
	userUpdatedTime: number;
	deletedTime: number;
};

export type NoteDetailDto = NoteSummaryDto & {
	body: string;
	markupLanguage: 'markdown'|'html';
	tagIds: string[];
};

export function folderDto(item: FolderEntity): FolderDto {
	return {
		id: item.id || '',
		parentId: item.parent_id || '',
		title: item.title || '',
		createdTime: item.created_time || 0,
		updatedTime: item.updated_time || 0,
		deletedTime: item.deleted_time || 0,
	};
}

export function tagDto(item: TagEntity, noteCount: number): TagDto {
	return {
		id: item.id || '',
		title: item.title || '',
		createdTime: item.created_time || 0,
		updatedTime: item.updated_time || 0,
		noteCount,
	};
}

export function noteSummaryDto(item: NoteEntity): NoteSummaryDto {
	return {
		id: item.id || '',
		parentId: item.parent_id || '',
		title: item.title || '',
		isTodo: !!item.is_todo,
		todoDue: item.todo_due || 0,
		todoCompleted: item.todo_completed || 0,
		createdTime: item.created_time || 0,
		updatedTime: item.updated_time || 0,
		userCreatedTime: item.user_created_time || 0,
		userUpdatedTime: item.user_updated_time || 0,
		deletedTime: item.deleted_time || 0,
	};
}

export function noteDetailDto(item: NoteEntity, tagIds: string[]): NoteDetailDto {
	return {
		...noteSummaryDto(item),
		body: item.body || '',
		markupLanguage: item.markup_language === 2 ? 'html' : 'markdown',
		tagIds: [...new Set(tagIds)].sort(),
	};
}
