import { useCallback, useEffect, useRef, useState, type ComponentType, type FormEvent } from 'react';
import { getRuntimeInfo, type RuntimeInfo } from './runtime';
import { open } from '@tauri-apps/plugin-dialog';
import {
	LibraryClientError, libraryApi, type Folder, type LibraryApi, type NoteDetail, type NoteSummary, type Resource, type SearchNote,
	type SyncConfig,
} from './library';
import RichTextEditor, { type RichTextEditorProps } from './RichTextEditor';
import './styles.css';

type Props = {
	loadRuntimeInfo?: ()=> Promise<RuntimeInfo>;
	library?: LibraryApi;
	EditorComponent?: ComponentType<RichTextEditorProps>;
};

type Initialization =
	| { kind: 'loading' }
	| { kind: 'ready'; runtime: RuntimeInfo }
	| { kind: 'failed'; message: string };

type SaveState = 'saved' | 'saving' | 'conflict' | 'failed';

const safeErrorMessage = (error: unknown, fallback: string) => {
	if (error instanceof LibraryClientError && error.code === 'CONFLICT') return '笔记已在别处修改，请重新载入';
	if (error instanceof LibraryClientError && error.code === 'SIDECAR_UNAVAILABLE') return '本地资料库不可用';
	if (error instanceof LibraryClientError && error.code === 'SYNC_NOT_CONFIGURED') return '尚未配置同步';
	if (error instanceof LibraryClientError && error.code === 'SYNC_AUTH_FAILED') return '同步认证失败';
	if (error instanceof LibraryClientError && error.code === 'SYNC_NETWORK') return '同步网络不可用';
	if (error instanceof LibraryClientError && error.code === 'SYNC_BUSY') return '同步正在进行';
	if (error instanceof LibraryClientError && error.code === 'SYNC_FAILED') return '同步失败';
	return fallback;
};

const noteLabel = (note: NoteSummary) => note.title.trim() || '未命名笔记';
const formatUpdatedTime = (time: number) => time > 0 ? new Intl.DateTimeFormat('zh-CN', { month: 'short', day: 'numeric' }).format(time) : '刚刚';

export default function App({
	loadRuntimeInfo = getRuntimeInfo,
	library = libraryApi,
	EditorComponent = RichTextEditor,
}: Props) {
	const [initialization, setInitialization] = useState<Initialization>({ kind: 'loading' });
	const [folders, setFolders] = useState<Folder[]>([]);
	const [notes, setNotes] = useState<NoteSummary[]>([]);
	const [searchQuery, setSearchQuery] = useState('');
	const [searchResults, setSearchResults] = useState<SearchNote[] | null>(null);
	const [searching, setSearching] = useState(false);
	const [searchError, setSearchError] = useState('');
	const searchSequence = useRef(0);
	const [selectedFolderId, setSelectedFolderId] = useState<string | null>(null);
	const [selectedNoteId, setSelectedNoteId] = useState<string | null>(null);
	const [detail, setDetail] = useState<NoteDetail | null>(null);
	const [resources, setResources] = useState<Resource[]>([]);
	const [syncConfig, setSyncConfig] = useState<SyncConfig>({ configured: false });
	const [syncFormOpen, setSyncFormOpen] = useState(false);
	const [syncUrl, setSyncUrl] = useState('');
	const [syncUsername, setSyncUsername] = useState('');
	const [syncPassword, setSyncPassword] = useState('');
	const [syncState, setSyncState] = useState<'idle'|'saving'|'syncing'|'success'|'failed'>('idle');
	const syncPollSequence = useRef(0);
	const [saveState, setSaveState] = useState<SaveState>('saved');
	const [errorMessage, setErrorMessage] = useState('');
	const [busy, setBusy] = useState(false);
	const detailRef = useRef<NoteDetail | null>(null);
	const draftRef = useRef<Partial<Pick<NoteDetail, 'title' | 'body'>>>({});
	const saveTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
	const saveQueue = useRef(Promise.resolve());
	const autoSaveDisabled = useRef(false);
	const saveSucceeded = useRef(true);

	const setCurrentDetail = useCallback((next: NoteDetail | null) => {
		detailRef.current = next;
		setDetail(next);
	}, []);

	const refreshNotes = useCallback(async (folderId: string | null = selectedFolderId) => {
		const page = await library.listNotes(folderId ? { parentId: folderId } : {});
		setNotes(page.items);
		return page.items;
	}, [library, selectedFolderId]);

	const bootstrap = useCallback(async (retry = false) => {
		syncPollSequence.current++;
		setInitialization({ kind: 'loading' });
		try {
			const [runtime] = await Promise.all([
				loadRuntimeInfo(), retry ? library.retryLibrary() : library.openLibrary(),
			]);
			const [loadedFolders, _loadedTags, page] = await Promise.all([
				library.listFolders(), library.listTags(), library.listNotes({}),
			]);
			setFolders(loadedFolders.filter(folder => folder.deletedTime === 0));
			setNotes(page.items);
			setSyncConfig(await library.getSyncConfig().catch(() => ({ configured: false })));
			setInitialization({ kind: 'ready', runtime });
		} catch (error) {
			setInitialization({ kind: 'failed', message: safeErrorMessage(error, '本地资料库不可用。没有修改现有笔记或资料库。') });
		}
	}, [library, loadRuntimeInfo]);

	useEffect(() => { void bootstrap(); }, [bootstrap]);

	useEffect(() => {
		if (initialization.kind !== 'ready') return;
		const query = searchQuery.trim();
		const requestSequence = ++searchSequence.current;
		if (!query) {
			setSearchResults(null);
			setSearching(false);
			setSearchError('');
			void refreshNotes();
			return;
		}
		setSearching(true);
		setSearchError('');
		const timer = setTimeout(() => {
			void library.searchNotes({ query, limit: 50 }).then(result => {
				if (requestSequence !== searchSequence.current) return;
				setSearchResults(result.items);
				setSearching(false);
			}).catch(() => {
				if (requestSequence !== searchSequence.current) return;
				setSearchResults([]);
				setSearching(false);
				setSearchError('搜索暂时不可用');
			});
		}, 250);
		return () => clearTimeout(timer);
	}, [initialization.kind, library, refreshNotes, searchQuery]);

	const enqueueSave = useCallback(() => {
		if (autoSaveDisabled.current) return;
		const draft = { ...draftRef.current };
		draftRef.current = {};
		if (!detailRef.current || Object.keys(draft).length === 0) return;
		setSaveState('saving');
		saveSucceeded.current = false;
		saveQueue.current = saveQueue.current.then(async () => {
			const current = detailRef.current;
			if (!current || autoSaveDisabled.current) return;
			try {
				const result = await library.updateNote({ id: current.id, expectedUpdatedTime: current.updatedTime, ...draft });
				const newerDraft = draftRef.current;
				setCurrentDetail({ ...result.item, ...newerDraft, updatedTime: result.item.updatedTime });
				setNotes(previous => previous.map(item => item.id === result.item.id ? { ...item, ...result.item, ...(newerDraft.title === undefined ? {} : { title: newerDraft.title }) } : item));
				saveSucceeded.current = true;
				setSaveState('saved');
			} catch (error) {
				draftRef.current = { ...draft, ...draftRef.current };
				saveSucceeded.current = false;
				autoSaveDisabled.current = true;
				setSaveState(error instanceof LibraryClientError && error.code === 'CONFLICT' ? 'conflict' : 'failed');
				setErrorMessage(safeErrorMessage(error, '保存失败，请重试'));
			}
		});
	}, [library, setCurrentDetail]);

	const flushSave = useCallback(async (): Promise<boolean> => {
		if (saveTimer.current) { clearTimeout(saveTimer.current); saveTimer.current = null; }
		if (Object.keys(draftRef.current).length > 0 && !autoSaveDisabled.current) enqueueSave();
		await saveQueue.current;
		return saveSucceeded.current && !autoSaveDisabled.current;
	}, [enqueueSave]);

	const scheduleSave = useCallback((change: Partial<Pick<NoteDetail, 'title' | 'body'>>) => {
		if (autoSaveDisabled.current) return;
		draftRef.current = { ...draftRef.current, ...change };
		if (saveTimer.current) clearTimeout(saveTimer.current);
		saveTimer.current = setTimeout(() => { saveTimer.current = null; enqueueSave(); }, 800);
	}, [enqueueSave]);

	const selectNote = useCallback(async (noteId: string) => {
		if (!await flushSave()) return;
		setBusy(true);
		try {
			const loaded = await library.getNote({ id: noteId });
			let loadedResources: Resource[] = [];
			let resourcesUnavailable = false;
			try { loadedResources = await library.listNoteResources({ noteId }); } catch { resourcesUnavailable = true; }
			setSelectedNoteId(noteId); setCurrentDetail(loaded); setResources(loadedResources); draftRef.current = {}; autoSaveDisabled.current = false;
			saveSucceeded.current = true; setSaveState('saved'); setErrorMessage(resourcesUnavailable ? '部分附件暂不可用' : '');
		} catch (error) { setErrorMessage(safeErrorMessage(error, '无法打开这篇笔记')); } finally { setBusy(false); }
	}, [flushSave, library, setCurrentDetail]);

	const selectFolder = useCallback(async (folderId: string | null) => {
		if (!await flushSave()) return;
		setSelectedFolderId(folderId); setSelectedNoteId(null); setCurrentDetail(null); setResources([]);
		try { await refreshNotes(folderId); } catch (error) { setErrorMessage(safeErrorMessage(error, '无法读取笔记')); }
	}, [flushSave, refreshNotes, setCurrentDetail]);

	const createNote = useCallback(async () => {
		if (!await flushSave()) return;
		setBusy(true); setErrorMessage('');
		try {
			let targetFolder = folders.find(folder => folder.id === selectedFolderId) ?? folders[0];
			if (!targetFolder) {
				const result = await library.createFolder({ parentId: '', title: '笔记' });
				targetFolder = result.item;
				setFolders(previous => [...previous, targetFolder!]);
			}
			const result = await library.createNote({ parentId: targetFolder.id, title: '', body: '' });
			setSelectedFolderId(targetFolder.id); setSelectedNoteId(result.item.id); setCurrentDetail(result.item); setResources([]);
			setNotes(previous => [result.item, ...previous.filter(item => item.id !== result.item.id)]);
			setSaveState('saved'); autoSaveDisabled.current = false; saveSucceeded.current = true;
		} catch (error) { setErrorMessage(safeErrorMessage(error, '无法新建笔记')); } finally { setBusy(false); }
	}, [folders, library, selectedFolderId, flushSave, setCurrentDetail]);

	const reloadNote = useCallback(async () => {
		if (!selectedNoteId) return;
		try {
			const loaded = await library.getNote({ id: selectedNoteId });
			let loadedResources: Resource[] = [];
			let resourcesUnavailable = false;
			try { loadedResources = await library.listNoteResources({ noteId: selectedNoteId }); } catch { resourcesUnavailable = true; }
			setCurrentDetail(loaded); setResources(loadedResources); draftRef.current = {}; autoSaveDisabled.current = false; saveSucceeded.current = true; setSaveState('saved'); setErrorMessage(resourcesUnavailable ? '部分附件暂不可用' : '');
		} catch (error) { setErrorMessage(safeErrorMessage(error, '无法重新载入笔记')); }
	}, [library, selectedNoteId, setCurrentDetail]);

	const configureSync = useCallback(async (event: FormEvent) => {
		event.preventDefault();
		setSyncState('saving'); setErrorMessage('');
		try {
			const config = await library.configureJoplinServer({ url: syncUrl, username: syncUsername, password: syncPassword });
			setSyncConfig(config); setSyncPassword(''); setSyncFormOpen(false); setSyncState('success');
		} catch (error) { setSyncState('failed'); setErrorMessage(safeErrorMessage(error, '同步配置失败')); }
	}, [library, syncPassword, syncUrl, syncUsername]);

	const runSync = useCallback(async () => {
		if (!await flushSave()) return;
		const pollSequence = ++syncPollSequence.current;
		setSyncState('syncing'); setErrorMessage('');
		try {
			let status = await library.startSync();
			while (status.state === 'running') {
				await new Promise<void>(resolve => setTimeout(resolve, 1000));
				if (pollSequence !== syncPollSequence.current) return;
				status = await library.getSyncStatus();
			}
			if (pollSequence !== syncPollSequence.current) return;
			if (status.state === 'failed') throw new LibraryClientError(status.code);
			const [loadedFolders, page] = await Promise.all([library.listFolders(), library.listNotes(selectedFolderId ? { parentId: selectedFolderId } : {})]);
			setFolders(loadedFolders.filter(folder => folder.deletedTime === 0)); setNotes(page.items);
			if (selectedNoteId) {
				const loaded = await library.getNote({ id: selectedNoteId });
				let loadedResources: Resource[] = [];
				try { loadedResources = await library.listNoteResources({ noteId: selectedNoteId }); } catch { setErrorMessage('部分附件暂不可用'); }
				setCurrentDetail(loaded); setResources(loadedResources);
			}
			setSyncState('success');
		} catch (error) { setSyncState('failed'); setErrorMessage(safeErrorMessage(error, '同步失败')); }
	}, [flushSave, library, selectedFolderId, selectedNoteId, setCurrentDetail]);

	const chooseResource = useCallback(async (): Promise<Resource | null> => {
		const selected = await open({ multiple: false, directory: false, title: '添加附件' });
		if (!selected || Array.isArray(selected)) return null;
		const title = selected.split(/[\\/]/).pop() || '附件';
		return library.createResourceFromPath({ path: selected, title });
	}, [library]);

	const openResource = useCallback(async (resource: Resource) => {
		try { await library.openResource({ id: resource.id, fileExtension: resource.fileExtension }); } catch (error) { setErrorMessage(safeErrorMessage(error, '无法打开附件')); }
	}, [library]);

	useEffect(() => () => { syncPollSequence.current++; void flushSave(); }, [flushSave]);

	if (initialization.kind === 'loading') return <main className="initialization-shell">正在打开本地资料库…</main>;
	if (initialization.kind === 'failed') return <section className="initialization-failure" role="alert"><p>{initialization.message}</p><button type="button" onClick={() => { void bootstrap(true); }}>重试打开资料库</button></section>;
	const visibleNotes: NoteSummary[] = searchResults ?? notes;

	return (
		<div className="app-shell">
			<nav className="navigation-rail" aria-label="导航">
				<p className="product-name">Joplin Lite</p>
				<button type="button" className={`navigation-item ${selectedFolderId === null ? 'is-active' : ''}`} aria-current={selectedFolderId === null ? 'page' : undefined} onClick={() => { void selectFolder(null); }}>全部笔记</button>
				<div className="folder-navigation"><p className="section-label">笔记本</p>{folders.map(folder => <button type="button" className="navigation-item" key={folder.id} aria-current={selectedFolderId === folder.id ? 'page' : undefined} onClick={() => { void selectFolder(folder.id); }}>{folder.title}</button>)}</div>
				<div className="sync-panel">
					<button type="button" className="sync-status" onClick={() => syncConfig.configured ? void runSync() : setSyncFormOpen(previous => !previous)} disabled={syncState === 'saving' || syncState === 'syncing'}>{syncState === 'syncing' ? '同步中…' : syncConfig.configured ? '同步' : '设置同步'}</button>
					{syncConfig.configured ? <button type="button" className="sync-settings" onClick={() => { setSyncUrl(syncConfig.url ?? ''); setSyncUsername(syncConfig.username ?? ''); setSyncPassword(''); setSyncFormOpen(true); }}>设置</button> : null}
					{syncConfig.configured && syncState === 'success' ? <small>刚刚同步</small> : null}
					{syncState === 'failed' ? <small role="status">同步失败，可重试</small> : null}
					{syncFormOpen ? <form className="sync-form" onSubmit={configureSync}>
						<label>服务器地址<input aria-label="服务器地址" value={syncUrl} onChange={event => setSyncUrl(event.target.value)} placeholder="https://…" required /></label>
						<label>邮箱<input aria-label="邮箱" type="email" value={syncUsername} onChange={event => setSyncUsername(event.target.value)} required /></label>
						<label>密码<input aria-label="密码" type="password" value={syncPassword} onChange={event => setSyncPassword(event.target.value)} required /></label>
						<button type="submit" disabled={syncState === 'saving'}>{syncState === 'saving' ? '连接中…' : '连接并保存'}</button>
					</form> : null}
				</div>
			</nav>
			<aside className="note-list" aria-label="笔记列表">
				<header className="pane-header"><div><p className="eyebrow">{selectedFolderId ? '笔记本' : '全部笔记'}</p><h2>笔记</h2></div><button type="button" className="new-note-button" onClick={() => { void createNote(); }} disabled={busy}>新建笔记</button></header>
				<div className="search-box"><label className="sr-only" htmlFor="note-search">搜索笔记</label><input id="note-search" aria-label="搜索笔记" value={searchQuery} onChange={event => setSearchQuery(event.target.value)} onKeyDown={event => { if (event.key === 'Escape') setSearchQuery(''); }} placeholder="搜索笔记" />{searching ? <small>搜索中…</small> : null}{searchError ? <small role="status">{searchError}</small> : null}</div>
				<div className="note-rows">{visibleNotes.map(item => <button type="button" className={`note-row ${selectedNoteId === item.id ? 'is-selected' : ''}`} key={item.id} aria-label={noteLabel(item)} onClick={() => { void selectNote(item.id); }}><strong>{noteLabel(item)}</strong><time>{formatUpdatedTime(item.updatedTime)}{'bodyMatch' in item && item.bodyMatch ? ' · 正文匹配' : ''}</time></button>)}{visibleNotes.length === 0 ? <p className="empty-note-list">{searchResults ? '没有找到匹配的笔记。' : <>这里会放下你正在写的东西。<br />从一页空白开始。</>}</p> : null}</div>
			</aside>
			<main className="editor-pane" aria-label="编辑区">
				{detail ? <>
					<header className="editor-header"><label className="sr-only" htmlFor="note-title">标题</label><input id="note-title" aria-label="标题" className="note-title-input" disabled={detail.markupLanguage === 'html'} value={detail.title} onChange={event => { const title = event.target.value; setCurrentDetail({ ...detail, title }); scheduleSave({ title }); }} placeholder="无标题" /><span className={`save-state save-state-${saveState}`}>{saveState === 'saved' ? '已保存' : saveState === 'saving' ? '保存中…' : saveState === 'conflict' ? '需要重载' : '保存失败'}</span></header>
					{detail.markupLanguage === 'html' ? <p className="editor-notice" role="status">此 HTML 笔记仅可在官方 Joplin 编辑。</p> : null}
					<EditorComponent noteId={detail.id} markdown={detail.body} resources={resources} onCreateImageResource={library.createImageResource} onChooseResource={chooseResource} onResourceCreated={resource => { setResources(previous => [...previous.filter(item => item.id !== resource.id), resource]); }} onOpenResource={openResource} readOnly={detail.markupLanguage === 'html'} onChange={body => { if (detail.markupLanguage === 'html') return; setCurrentDetail({ ...detail, body }); scheduleSave({ body }); }} />
					{errorMessage ? <div className="editor-error" role="alert"><span>{errorMessage}</span>{saveState === 'conflict' ? <button type="button" onClick={() => { void reloadNote(); }}>重新载入</button> : <button type="button" onClick={() => { autoSaveDisabled.current = false; setErrorMessage(''); scheduleSave(draftRef.current); }}>重试保存</button>}</div> : null}
				</> : <section className="editor-empty"><p className="eyebrow">一页空白</p><h1>把想法放下来。</h1><p>选择一篇笔记，或创建一篇新的。</p><button type="button" className="new-note-button" onClick={() => { void createNote(); }}>新建笔记</button></section>}
			</main>
		</div>
	);
}
