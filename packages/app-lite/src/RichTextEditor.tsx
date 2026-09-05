import { useEffect, useMemo, useRef, useState } from 'react';
import { createEditor } from '@joplin/editor/ProseMirror';
import { EditorEventType, type EditorEvent } from '@joplin/editor/events';
import { EditorKeymap, EditorLanguageType, UserEventSource, type EditorControl, type EditorProps, type EditorSettings } from '@joplin/editor/types';
import type { OnCreateCodeEditor, RendererControl } from '@joplin/editor/ProseMirror/types';
import { MarkupToHtml, MarkupLanguage } from '@joplin/renderer';
// These packages intentionally mirror Joplin mobile's renderer adapter. They do not ship
// declarations, so the runtime package is kept behind this narrow, typed boundary.
import TurndownService from '@joplin/turndown';
import { gfm } from '@joplin/turndown-plugin-gfm';
import '@joplin/editor/ProseMirror/styles';
import { createImageFromFile } from './resource';
import { resourceUrl, type Resource, type CreateImageResourceParams } from './library';

export interface RichTextEditorProps {
	noteId: string;
	markdown: string;
	onChange: (markdown: string)=> void;
	resources?: Resource[];
	onCreateImageResource?: (params: CreateImageResourceParams)=> Promise<Resource>;
	onChooseResource?: ()=> Promise<Resource | null>;
	onResourceCreated?: (resource: Resource)=> void;
	onOpenResource?: (resource: Resource)=> Promise<void> | void;
	ariaLabel?: string;
	readOnly?: boolean;
}

const rendererTheme = {
	appearance: 'light', color: '#2a3035', backgroundColor: '#fbfaf7', codeBackgroundColor: '#f0eee8',
	codeBorderColor: '#d9d5ce', codeColor: '#24454b', dividerColor: '#d9d5ce', raisedBackgroundColor: '#fffefa',
	urlColor: '#1b6870', tableBackgroundColor: '#f5f2eb', fontSize: 15, lineHeight: '1.65',
};

const browserResourceModel = {
	isResourceUrl: (url: string) => /^:\/[0-9a-f]{32}$/.test(url),
	urlToId: (url: string) => url.slice(2),
	filename: (resource: { id?: string; file_extension?: string }) => `${resource.id || ''}${resource.file_extension ? `.${resource.file_extension}` : ''}`,
	isSupportedImageMimeType: (mime: string) => ['image/png', 'image/jpeg', 'image/gif', 'image/webp', 'image/avif', 'image/bmp'].includes(mime.toLowerCase()),
};
const markdownRenderer = new MarkupToHtml({ isSafeMode: false, ResourceModel: browserResourceModel });

export async function markdownToHtml(markdown: string, resources: Resource[] = []): Promise<string> {
	const resourceMap = Object.fromEntries(resources.map(resource => [resource.id, {
		item: { id: resource.id, title: resource.title, mime: resource.mime, file_extension: resource.fileExtension, size: resource.size, updated_time: resource.updatedTime },
		localState: { fetch_status: 2 },
	}]));
	const result = await markdownRenderer.render(
		MarkupLanguage.Markdown,
		markdown,
		rendererTheme,
		{
			bodyOnly: true, theme: rendererTheme, platformName: 'desktop', resources: resourceMap,
			itemIdToUrl: (id: string) => {
				const resource = resources.find(item => item.id === id);
				return resource ? resourceUrl(resource) : '';
			},
		},
	);
	return result.html;
}

export function htmlToMarkdown(html: string | HTMLElement): string {
	const container = document.createElement('div');
	if (typeof html === 'string') container.innerHTML = html;
	else container.append(...Array.from(html.childNodes).map(node => node.cloneNode(true)));
	const turndown = new TurndownService({
		headingStyle: 'atx', codeBlockStyle: 'fenced', bulletListMarker: '-',
		emDelimiter: '*', strongDelimiter: '**', allowResourcePlaceholders: true, br: '  ',
	});
	turndown.use(gfm);
	turndown.remove('script');
	turndown.remove('style');
	return turndown.turndown(container);
}

const editorSettings: EditorSettings = {
	themeData: {
		...rendererTheme, themeId: 1, fontFamily: '-apple-system, BlinkMacSystemFont, sans-serif',
		fontSize: 15, paddingBottom: 96, appearance: 'light', backgroundColorTransparent: '#fbfaf700',
		oddBackgroundColor: '#f5f2eb', colorError: '#9b3b34', colorErrorSelected: '#f3d6d1', colorCorrect: '#24725e',
		colorWarn: '#8d6518', colorWarnUrl: '#8d6518', colorFaded: '#6f756f', dividerColor: '#d9d5ce',
		selectedColor: '#dcebea', urlColor: '#1b6870', shadowColor: '#00000018', backgroundColor2: '#e9e5dd',
		backgroundColorTransparent2: '#00000000', color2: '#2a3035', selectedColor2: '#dcebea', colorError2: '#9b3b34',
		colorWarn2: '#8d6518', colorWarn3: '#8d6518', color3: '#2a3035', backgroundColor3: '#f5f2eb',
		backgroundColorHover3: '#ebe7df', color4: '#1b6870', backgroundColor4: '#fffefa', backgroundColor4Dimmed: '#f0eee8',
		raisedBackgroundColor: '#fffefa', raisedColor: '#2a3035', searchMarkerBackgroundColor: '#f2d36b', searchMarkerColor: '#2a3035',
		warningBackgroundColor: '#f7edcf', destructiveColor: '#9b3b34', tableBackgroundColor: '#f5f2eb',
		codeBackgroundColor: '#f0eee8', codeBorderColor: '#d9d5ce', codeColor: '#24454b', blockQuoteOpacity: 0.72,
		codeMirrorTheme: 'default', codeThemeCss: '', headerBackgroundColor: '#eeece7', textSelectionColor: '#cfe4e2',
		colorBright2: '#ffffff', isDesktop: true,
	},
	useExternalSearch: true, automatchBraces: true, autocompleteMarkup: false, ignoreModifiers: false,
	language: EditorLanguageType.Markdown, keymap: EditorKeymap.Default, preferMacShortcuts: true, tabMovesFocus: false,
	markdownMarkEnabled: true, markdownInsertEnabled: true, katexEnabled: false, spellcheckEnabled: true,
	inlineRenderingEnabled: true, tableEditingEnabled: true, imageRenderingEnabled: true, readOnly: false,
	highlightActiveLine: false, indentWithTabs: false, editorLabel: '正文',
};

function createCodeEditor(parent: HTMLElement, _language: unknown, onChange: (value: string)=> void): ReturnType<OnCreateCodeEditor> {
	const textarea = document.createElement('textarea');
	textarea.className = 'rich-text-code-editor';
	textarea.setAttribute('aria-label', '代码块');
	textarea.spellcheck = false;
	textarea.addEventListener('input', () => onChange(textarea.value));
	parent.appendChild(textarea);
	return {
		focus: () => textarea.focus(), remove: () => textarea.remove(), updateBody: value => { textarea.value = value; },
		select: (from, to) => textarea.setSelectionRange(from, to),
	};
}

function createRenderer(resourcesRef: { current: Resource[] }): RendererControl {
	return {
		renderMarkupToHtml: async (markup, _options) => ({ html: await markdownToHtml(markup, resourcesRef.current), cssStrings: [], pluginAssets: [] }),
		renderHtmlToMarkup: htmlToMarkdown,
	};
}

const commandButtons = [
	['textBold', '粗体'], ['textItalic', '斜体'], ['textHeading2', '二级标题'],
	['textBulletedList', '项目列表'], ['textCheckbox', '待办'], ['textCodeBlock', '代码块'],
] as const;

export default function RichTextEditor({ noteId, markdown, onChange, resources = [], onCreateImageResource, onChooseResource, onResourceCreated, onOpenResource, ariaLabel = '正文', readOnly = false }: RichTextEditorProps) {
	const host = useRef<HTMLDivElement>(null);
	const editor = useRef<EditorControl | null>(null);
	const latestMarkdown = useRef(markdown);
	const onChangeRef = useRef(onChange);
	const resourcesRef = useRef(resources);
	const createImageRef = useRef(onCreateImageResource);
	const chooseResourceRef = useRef(onChooseResource);
	const resourceCreatedRef = useRef(onResourceCreated);
	const openResourceRef = useRef(onOpenResource);
	const [pasteMessage, setPasteMessage] = useState('');
	const [attachmentBusy, setAttachmentBusy] = useState(false);
	const [editorReady, setEditorReady] = useState(false);
	resourcesRef.current = resources;
	createImageRef.current = onCreateImageResource;
	chooseResourceRef.current = onChooseResource;
	resourceCreatedRef.current = onResourceCreated;
	openResourceRef.current = onOpenResource;
	const renderer = useMemo(() => createRenderer(resourcesRef), []);
	onChangeRef.current = onChange;

	useEffect(() => {
		let mounted = true;
		if (!host.current) return undefined;
		latestMarkdown.current = markdown;
		const insertResource = (resource: Resource, source: UserEventSource) => {
			resourcesRef.current = [...resourcesRef.current.filter(item => item.id !== resource.id), resource];
			resourceCreatedRef.current?.(resource);
			editor.current?.insertText(resource.markup, source);
		};
		const props: EditorProps = {
			settings: { ...editorSettings, editorLabel: ariaLabel, readOnly }, initialText: markdown, initialNoteId: noteId,
			onLocalize: input => input, onPasteFile: async file => {
				if (!createImageRef.current || readOnly) return;
				setAttachmentBusy(true); setPasteMessage('');
				try { insertResource(await createImageFromFile(file, createImageRef.current), UserEventSource.Paste); } catch (error) { setPasteMessage(error instanceof Error ? error.message : '附件添加失败'); } finally { setAttachmentBusy(false); }
			},
			onEvent: (event: unknown) => {
				const change = event as EditorEvent;
				if (change.kind === EditorEventType.Change) {
					latestMarkdown.current = change.value;
					onChangeRef.current(change.value);
				}
				if (change.kind === EditorEventType.FollowLink && change.link.startsWith(':/')) {
					const resourceId = change.link.slice(2).split(/[?#]/, 1)[0];
					const resource = resourcesRef.current.find(item => item.id === resourceId);
					if (resource) void openResourceRef.current?.(resource);
				}
			}, onLogMessage: () => {},
		};
		void createEditor(host.current, props, renderer, createCodeEditor).then(control => {
			if (mounted) { editor.current = control; setEditorReady(true); } else { control.remove(); }
		});
		return () => {
			mounted = false;
			editor.current?.remove();
			editor.current = null;
		};
	}, [ariaLabel, noteId, readOnly, renderer]);

	useEffect(() => {
		if (!editor.current || markdown === latestMarkdown.current) return;
		latestMarkdown.current = markdown;
		void editor.current.updateBody(markdown);
	}, [markdown]);

	return (
		<section className="rich-text-editor" aria-label={ariaLabel}>
			<div className="editor-toolbar" role="toolbar" aria-label="格式工具">
				{commandButtons.map(([command, label]) => (
					<button key={command} type="button" className="toolbar-button" aria-label={label}
						disabled={!editorReady || readOnly} onClick={() => { void editor.current?.execCommand(command); }}>
						{label}
					</button>
				))}
				<button type="button" className="toolbar-button" aria-label="附件" disabled={!editorReady || readOnly || attachmentBusy || !chooseResourceRef.current} onClick={async () => {
					if (!chooseResourceRef.current) return;
					setAttachmentBusy(true); setPasteMessage('');
					try {
						const resource = await chooseResourceRef.current();
						if (resource) {
							resourcesRef.current = [...resourcesRef.current.filter(item => item.id !== resource.id), resource];
							resourceCreatedRef.current?.(resource);
							editor.current?.insertText(resource.markup, UserEventSource.Paste);
						}
					} catch (error) { setPasteMessage(error instanceof Error ? error.message : '附件添加失败'); } finally { setAttachmentBusy(false); }
				}}>{attachmentBusy ? '添加中…' : '附件'}</button>
			</div>
			<div className="editor-host" ref={host} />
			{pasteMessage ? <p className="editor-notice" role="status">{pasteMessage}</p> : null}
		</section>
	);
}
