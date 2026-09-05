import type { ResourceEntity } from '../../../lib/services/database/types';
import Resource from '../../../lib/models/Resource';
import Note from '../../../lib/models/Note';
import NoteResource from '../../../lib/models/NoteResource';
import shim from '../../../lib/shim';
import { notFoundError, validateId, validationError } from './validation';

const MARKUP_LANGUAGE_MARKDOWN = 1;

export type ResourceDto = {
	id: string;
	title: string;
	mime: string;
	fileExtension: string;
	size: number;
	createdTime: number;
	updatedTime: number;
	markup: string;
};

export type CreateResourceInput = { path: unknown; title?: unknown };
export type ResourceCreateResult = ResourceDto;

type ResourceModel = Pick<typeof Resource, 'markupTag'|'load'>;
type NoteModel = Pick<typeof Note, 'load'>;
type NoteResourceModel = Pick<typeof NoteResource, 'associatedResourceIds'>;
type ShimModel = Pick<typeof shim, 'createResourceFromPath'>;
type ResourceStoreOptions = {
	resource?: ResourceModel;
	note?: NoteModel;
	noteResource?: NoteResourceModel;
	shim?: ShimModel;
};

function resourceDto(resource: ResourceEntity, markup: string): ResourceDto {
	const id = validateId(resource.id);
	return {
		id,
		title: resource.title || '',
		mime: resource.mime || 'application/octet-stream',
		fileExtension: resource.file_extension || '',
		size: Number.isSafeInteger(resource.size) && resource.size >= 0 ? resource.size : 0,
		createdTime: Number.isSafeInteger(resource.created_time) && resource.created_time >= 0 ? resource.created_time : 0,
		updatedTime: Number.isSafeInteger(resource.updated_time) && resource.updated_time >= 0 ? resource.updated_time : 0,
		markup,
	};
}

export class ResourceStore {
	private readonly resource: ResourceModel;
	private readonly note: NoteModel;
	private readonly noteResource: NoteResourceModel;
	private readonly shim: ShimModel;

	public constructor(options: ResourceStoreOptions = {}) {
		this.resource = options.resource ?? Resource;
		this.note = options.note ?? Note;
		this.noteResource = options.noteResource ?? NoteResource;
		this.shim = options.shim ?? shim;
	}

	public async createFromPath(input: CreateResourceInput): Promise<ResourceCreateResult> {
		if (!input || typeof input !== 'object' || typeof input.path !== 'string' || !input.path) throw validationError();
		if (input.title !== undefined && typeof input.title !== 'string') throw validationError();
		const title = typeof input.title === 'string' ? input.title : undefined;
		if (title !== undefined && (title.length > 4096 || title.includes('\0'))) throw validationError();
		try {
			const resource = await this.shim.createResourceFromPath(
				input.path,
				title === undefined ? null : { title },
				{ resizeLargeImages: 'never', userSideValidation: true },
			);
			if (!resource?.id) throw validationError();
			const markup = this.resource.markupTag(resource, MARKUP_LANGUAGE_MARKDOWN);
			return resourceDto(resource, markup);
		} catch (error) {
			if (error && typeof error === 'object' && 'code' in error) throw error;
			throw validationError();
		}
	}

	public async listForNote(noteIdValue: unknown): Promise<ResourceDto[]> {
		const noteId = validateId(noteIdValue);
		const note = await this.note.load(noteId);
		if (!note) throw notFoundError();
		try {
			const ids = (await this.noteResource.associatedResourceIds(noteId)).map(validateId);
			const resources = await Promise.all([...new Set(ids)].map(id => this.resource.load(id)));
			return resources.filter((resource): resource is ResourceEntity => !!resource).map(resource => resourceDto(resource, this.resource.markupTag(resource, MARKUP_LANGUAGE_MARKDOWN)));
		} catch (error) {
			if (error && typeof error === 'object' && 'code' in error) throw error;
			throw validationError();
		}
	}
}
