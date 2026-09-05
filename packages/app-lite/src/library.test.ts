import { invoke } from '@tauri-apps/api/core';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createFolder, listNotes, openLibrary } from './library';

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
});
