import { invoke } from '@tauri-apps/api/core';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createFolder, createImageResource, createResourceFromPath, getSyncStatus, listNoteResources, listNotes, openLibrary, resourceUrl, startSync } from './library';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

describe('library invoke client', () => {
	beforeEach(() => vi.mocked(invoke).mockReset());

	it('passes typed command parameters and guards folder results', async () => {
		const folder = {
			id: '0123456789abcdef0123456789abcdef',
			parentId: '',
			title: 'Inbox',
			createdTime: 1,
			updatedTime: 2,
			deletedTime: 0,
		};
		vi.mocked(invoke).mockResolvedValue({
			created: true,
			item: folder,
		});

		expect(await createFolder({ parentId: '', title: 'Inbox' })).toEqual({ created: true, item: folder });
		expect(invoke).toHaveBeenCalledExactlyOnceWith('create_folder', {
			params: { parentId: '', title: 'Inbox' },
		});
	});

	it('rejects malformed note pages with a fixed client error', async () => {
		vi.mocked(invoke).mockResolvedValue({ items: [{ id: 'bad' }], page: 1, hasMore: false });

		await expect(listNotes()).rejects.toThrow('本地资料库响应无效');
	});

	it('always sends the required params object for the default note page', async () => {
		vi.mocked(invoke).mockResolvedValue({ items: [], page: 1, hasMore: false });

		await expect(listNotes()).resolves.toEqual({ items: [], page: 1, hasMore: false });
		expect(invoke).toHaveBeenCalledExactlyOnceWith('list_notes', { params: {} });
	});

	it('guards the open profile response', async () => {
		vi.mocked(invoke).mockResolvedValue({ state: 'open', schemaVersion: 53, formatVersion: 1 });

		expect(await openLibrary()).toEqual({ state: 'open', schemaVersion: 53, formatVersion: 1 });
		vi.mocked(invoke).mockResolvedValue({ state: 'open', schemaVersion: -1, formatVersion: 1 });
		await expect(openLibrary()).rejects.toThrow('本地资料库响应无效');
	});

	it('guards the non-blocking sync lifecycle status', async () => {
		vi.mocked(invoke).mockResolvedValueOnce({ state: 'running' });
		await expect(startSync()).resolves.toEqual({ state: 'running' });
		vi.mocked(invoke).mockResolvedValueOnce({ state: 'failed', code: 'SYNC_NETWORK' });
		await expect(getSyncStatus()).resolves.toEqual({ state: 'failed', code: 'SYNC_NETWORK' });
		vi.mocked(invoke).mockResolvedValueOnce({ state: 'failed', code: 'SECRET_ERROR' });
		await expect(getSyncStatus()).rejects.toThrow('本地资料库响应无效');
	});

	it('guards and sends official resource DTOs without exposing paths', async () => {
		const resource = { id: 'a'.repeat(32), title: 'picture.png', mime: 'image/png', fileExtension: 'png', size: 3, createdTime: 1, updatedTime: 2, markup: '![]( :/bad )' };
		vi.mocked(invoke).mockResolvedValue(resource);
		await expect(createResourceFromPath({ path: '/trusted/picture.png' })).resolves.toEqual(resource);
		expect(invoke).toHaveBeenCalledWith('create_resource_from_path', { params: { path: '/trusted/picture.png' } });
		vi.mocked(invoke).mockResolvedValue([resource]);
		await expect(listNoteResources({ noteId: 'b'.repeat(32) })).resolves.toEqual([resource]);
	});

	it('rejects malformed resources and creates a controlled resource URL', async () => {
		vi.mocked(invoke).mockResolvedValue({ id: 'a'.repeat(32), title: '', mime: 'image/png', fileExtension: 'png', size: 1, createdTime: 1, updatedTime: 1, markup: '', secret: '/tmp/x' });
		await expect(createImageResource({ title: 'x.png', mime: 'image/png', base64: 'eA==' })).rejects.toThrow('本地资料库响应无效');
		expect(resourceUrl({ id: 'a'.repeat(32), fileExtension: 'png', updatedTime: 2 })).toBe('joplin-resource://localhost/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.png?t=2');
		expect(() => resourceUrl({ id: '../bad', fileExtension: 'png', updatedTime: 2 })).toThrow('本地资料库响应无效');
	});
});
