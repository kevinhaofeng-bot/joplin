import type { FolderEntity, TagEntity } from '../../../lib/services/database/types';

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
