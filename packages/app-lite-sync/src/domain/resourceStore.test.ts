import { ResourceStore } from './resourceStore';

const resourceId = 'a'.repeat(32);
const noteId = 'b'.repeat(32);

describe('resource store', () => {
	test('creates an official resource with a markdown link without exposing its path', async () => {
		const createResourceFromPath = jest.fn(async (_path: string, defaults: any) => ({
			id: resourceId, title: defaults?.title || 'picture.png', mime: 'image/png', file_extension: 'png', size: 12, created_time: 3, updated_time: 4,
		}));
		const store = new ResourceStore({
			shim: { createResourceFromPath } as any,
			resource: { markupTag: (resource: any) => `![${resource.title}](:/${resource.id})` } as any,
		});

		await expect(store.createFromPath({ path: '/trusted/input/picture.png', title: '插图' })).resolves.toEqual({
			id: resourceId, title: '插图', mime: 'image/png', fileExtension: 'png', size: 12, createdTime: 3, updatedTime: 4,
			markup: `![插图](:/${resourceId})`,
		});
		expect(createResourceFromPath).toHaveBeenCalledWith('/trusted/input/picture.png', { title: '插图' }, {
			resizeLargeImages: 'never', userSideValidation: true,
		});
		expect(JSON.stringify(await store.createFromPath({ path: '/trusted/input/picture.png' }))).not.toContain('/trusted/input');
	});

	test('lists associated official resource metadata without exposing paths', async () => {
		const store = new ResourceStore({
			note: { load: async () => ({ id: noteId, body: `![x](:/${resourceId})` }) } as any,
			noteResource: { associatedResourceIds: async () => [resourceId] } as any,
			resource: { load: async () => ({ id: resourceId, title: 'x.pdf', mime: 'application/pdf', size: 5, file_extension: 'pdf', created_time: 1, updated_time: 2 }), markupTag: jest.fn(() => '') } as any,
		});

		await expect(store.listForNote(noteId)).resolves.toEqual([{ id: resourceId, title: 'x.pdf', mime: 'application/pdf', fileExtension: 'pdf', size: 5, createdTime: 1, updatedTime: 2, markup: '' }]);
	});
});
