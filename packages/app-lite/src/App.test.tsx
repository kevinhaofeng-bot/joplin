import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import App from './App';
import { LibraryClientError, type LibraryApi, type NoteDetail } from './library';
import type { RichTextEditorProps } from './RichTextEditor';

const folder = {
	id: 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', parentId: '', title: '笔记',
	createdTime: 1, updatedTime: 1, deletedTime: 0,
};
const note: NoteDetail = {
	id: 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', parentId: folder.id, title: '', body: '',
	markupLanguage: 'markdown', tagIds: [], isTodo: false, todoDue: 0, todoCompleted: 0,
	createdTime: 1, updatedTime: 1, userCreatedTime: 1, userUpdatedTime: 1, deletedTime: 0,
};

function makeApi(overrides: Partial<LibraryApi> = {}): LibraryApi {
	return {
		openLibrary: vi.fn(async () => ({ state: 'open' as const, schemaVersion: 53, formatVersion: 1 })),
		retryLibrary: vi.fn(async () => ({ state: 'open' as const, schemaVersion: 53, formatVersion: 1 })),
		profileStatus: vi.fn(async () => ({ state: 'open' as const, formatVersion: 1 })),
		shutdownLibrary: vi.fn(async () => undefined),
		listFolders: vi.fn(async () => []), createFolder: vi.fn(async () => ({ created: true, item: folder })),
		updateFolder: vi.fn(), trashFolder: vi.fn(), listTags: vi.fn(async () => []),
		createTag: vi.fn(), updateTag: vi.fn(), deleteTag: vi.fn(),
		listNotes: vi.fn(async () => ({ items: [], page: 1, hasMore: false })), searchNotes: vi.fn(async () => ({ query: '', items: [] })), getNote: vi.fn(async () => note),
		createNote: vi.fn(async () => ({ created: true, item: note })),
		updateNote: vi.fn(async (params) => ({ changed: true, item: { ...note, ...params, updatedTime: params.expectedUpdatedTime + 1 } })),
		trashNote: vi.fn(), setNoteTags: vi.fn(), createResourceFromPath: vi.fn(), listNoteResources: vi.fn(async () => []),
		createImageResource: vi.fn(), openResource: vi.fn(), getSyncConfig: vi.fn(async () => ({ configured: false })),
		configureJoplinServer: vi.fn(async () => ({ configured: true, url: 'https://sync.example.test', username: 'user@example.test' })), syncNow: vi.fn(async () => ({ completedAt: 1, created: 1, updated: 0, deleted: 0, fetched: 0 })), ...overrides,
	};
}

function FakeEditor({ markdown, onChange }: RichTextEditorProps) {
	return <textarea aria-label="正文" value={markdown} onChange={event => onChange(event.target.value)} />;
}

const runtime = async () => ({ appName: 'Joplin Lite', profileDirectory: '/tmp/joplin-lite-test' });

describe('App', () => {
	afterEach(() => cleanup());
	it('creates a default folder and note, selects it, then serially debounces title/body updates', async () => {
		const api = makeApi();
		render(<App loadRuntimeInfo={runtime} library={api} EditorComponent={FakeEditor} />);

		await screen.findAllByRole('button', { name: '新建笔记' });
		fireEvent.click(screen.getAllByRole('button', { name: '新建笔记' })[0]);
		await screen.findByRole('textbox', { name: '标题' });
		expect(api.createFolder).toHaveBeenCalledWith({ parentId: '', title: '笔记' });
		expect(api.createNote).toHaveBeenCalledWith({ parentId: folder.id, title: '', body: '' });

		fireEvent.change(screen.getByRole('textbox', { name: '标题' }), { target: { value: '第一篇' } });
		fireEvent.change(screen.getByRole('textbox', { name: '正文' }), { target: { value: '正文内容' } });
		await new Promise(resolve => setTimeout(resolve, 900));
		await waitFor(() => expect(api.updateNote).toHaveBeenCalled());
		expect(api.updateNote).toHaveBeenLastCalledWith(expect.objectContaining({
			id: note.id, expectedUpdatedTime: note.updatedTime, title: '第一篇', body: '正文内容',
		}));
	});

	it('stops automatic overwrites after a conflict and offers reload', async () => {
		const api = makeApi({
			listNotes: vi.fn(async () => ({ items: [note], page: 1, hasMore: false })),
			getNote: vi.fn(async () => note),
			updateNote: vi.fn(async () => { throw new LibraryClientError('CONFLICT', '项目已被其他操作修改'); }),
		});
		render(<App loadRuntimeInfo={runtime} library={api} EditorComponent={FakeEditor} />);

		const noteButton = await screen.findByRole('button', { name: '未命名笔记' });
		fireEvent.click(noteButton);
		await screen.findByRole('textbox', { name: '标题' });
		fireEvent.change(screen.getByRole('textbox', { name: '标题' }), { target: { value: '覆盖尝试' } });
		await new Promise(resolve => setTimeout(resolve, 950));
		await screen.findByText('笔记已在别处修改，请重新载入');
		expect(screen.getByRole('button', { name: '重新载入' })).toBeInTheDocument();
	});

	it('fails closed with a retry action when opening the library fails', async () => {
		const api = makeApi({ openLibrary: vi.fn(async () => { throw new LibraryClientError('SIDECAR_UNAVAILABLE', '本地资料库不可用'); }) });
		render(<App loadRuntimeInfo={runtime} library={api} EditorComponent={FakeEditor} />);

		await screen.findByRole('alert');
		expect(screen.getByRole('button', { name: '重试打开资料库' })).toBeInTheDocument();
	});

	it('keeps the note open when resource metadata is temporarily unavailable', async () => {
		const api = makeApi({
			listNotes: vi.fn(async () => ({ items: [note], page: 1, hasMore: false })),
			getNote: vi.fn(async () => note),
			listNoteResources: vi.fn(async () => { throw new LibraryClientError('STORAGE_ERROR', '无法保存资料库'); }),
		});
		render(<App loadRuntimeInfo={runtime} library={api} EditorComponent={FakeEditor} />);
		fireEvent.click(await screen.findByRole('button', { name: '未命名笔记' }));
		expect(await screen.findByRole('textbox', { name: '正文' })).toHaveValue('');
		expect(await screen.findByText('部分附件暂不可用')).toBeInTheDocument();
	});

	it('configures manual sync and refreshes the current note after syncing', async () => {
		const api = makeApi({ listNotes: vi.fn(async () => ({ items: [note], page: 1, hasMore: false })) });
		render(<App loadRuntimeInfo={runtime} library={api} EditorComponent={FakeEditor} />);
		await screen.findByRole('button', { name: '设置同步' });
		fireEvent.click(screen.getByRole('button', { name: '设置同步' }));
		fireEvent.change(screen.getByLabelText('服务器地址'), { target: { value: 'https://sync.example.test' } });
		fireEvent.change(screen.getByLabelText('邮箱'), { target: { value: 'user@example.test' } });
		fireEvent.change(screen.getByLabelText('密码'), { target: { value: 'secret-password' } });
		fireEvent.click(screen.getByRole('button', { name: '连接并保存' }));
		await screen.findByRole('button', { name: '同步' });
		fireEvent.click(screen.getByRole('button', { name: '同步' }));
		await waitFor(() => expect(api.syncNow).toHaveBeenCalledTimes(1));
		expect(api.listFolders).toHaveBeenCalled();
		expect(api.listNotes).toHaveBeenCalled();
	});

	it('debounces note search and Escape restores the regular list', async () => {
		const api = makeApi({
			listNotes: vi.fn(async () => ({ items: [note], page: 1, hasMore: false })),
			searchNotes: vi.fn(async () => ({ query: 'body', items: [{ ...note, bodyMatch: true }] })),
		});
		render(<App loadRuntimeInfo={runtime} library={api} EditorComponent={FakeEditor} />);
		const search = await screen.findByRole('textbox', { name: '搜索笔记' });
		fireEvent.change(search, { target: { value: 'body' } });
		await waitFor(() => expect(api.searchNotes).toHaveBeenCalledWith({ query: 'body', limit: 50 }), { timeout: 1000 });
		await screen.findByText(/正文匹配/);
		fireEvent.keyDown(search, { key: 'Escape' });
		await waitFor(() => expect(search).toHaveValue(''));
	});
});
