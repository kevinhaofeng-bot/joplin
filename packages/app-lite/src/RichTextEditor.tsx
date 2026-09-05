import { useEffect, useMemo, useRef, useState } from 'react';
import { createEditor } from '@joplin/editor/ProseMirror';
import { EditorEventType, type EditorEvent } from '@joplin/editor/events';
import { EditorKeymap, EditorLanguageType, type EditorControl, type EditorProps, type EditorSettings } from '@joplin/editor/types';
import type { OnCreateCodeEditor, RendererControl } from '@joplin/editor/ProseMirror/types';
import { MarkupToHtml, MarkupLanguage } from '@joplin/renderer';
// These packages intentionally mirror Joplin mobile's renderer adapter. They do not ship
// declarations, so the runtime package is kept behind this narrow, typed boundary.
import TurndownService from '@joplin/turndown';
import { gfm } from '@joplin/turndown-plugin-gfm';
import '@joplin/editor/ProseMirror/styles';

export interface RichTextEditorProps {
	noteId: string;
	markdown: string;
	onChange: (markdown: string)=> void;
	ariaLabel?: string;
	readOnly?: boolean;
}

const rendererTheme = {
	appearance: 'light', color: '#2a3035', backgroundColor: '#fbfaf7', codeBackgroundColor: '#f0eee8',
	codeBorderColor: '#d9d5ce', codeColor: '#24454b', dividerColor: '#d9d5ce', raisedBackgroundColor: '#fffefa',
	urlColor: '#1b6870', tableBackgroundColor: '#f5f2eb', fontSize: 15, lineHeight: '1.65',
};

const markdownRenderer = new MarkupToHtml({ isSafeMode: false });

export async function markdownToHtml(markdown: string): Promise<string> {
	const result = await markdownRenderer.render(
		MarkupLanguage.Markdown,
		markdown,
		rendererTheme,
		{ bodyOnly: true, theme: rendererTheme, platformName: 'desktop' },
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
	inlineRenderingEnabled: true, tableEditingEnabled: true, imageRenderingEnabled: false, readOnly: false,
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

function createRenderer(): RendererControl {
	return {
		renderMarkupToHtml: async (markup, _options) => ({ html: await markdownToHtml(markup), cssStrings: [], pluginAssets: [] }),
		renderHtmlToMarkup: htmlToMarkdown,
	};
}

const commandButtons = [
	['textBold', '粗体'], ['textItalic', '斜体'], ['textHeading2', '二级标题'],
	['textBulletedList', '项目列表'], ['textCheckbox', '待办'], ['textCodeBlock', '代码块'],
] as const;

export default function RichTextEditor({ noteId, markdown, onChange, ariaLabel = '正文', readOnly = false }: RichTextEditorProps) {
	const host = useRef<HTMLDivElement>(null);
	const editor = useRef<EditorControl | null>(null);
	const latestMarkdown = useRef(markdown);
	const onChangeRef = useRef(onChange);
	const [pasteMessage, setPasteMessage] = useState('');
	const [editorReady, setEditorReady] = useState(false);
	const renderer = useMemo(createRenderer, []);
	onChangeRef.current = onChange;

	useEffect(() => {
		let mounted = true;
		if (!host.current) return undefined;
		latestMarkdown.current = markdown;
		const props: EditorProps = {
			settings: { ...editorSettings, editorLabel: ariaLabel, readOnly }, initialText: markdown, initialNoteId: noteId,
			onLocalize: input => input, onPasteFile: async () => { setPasteMessage('附件将在下一里程碑接入'); },
			onEvent: (event: unknown) => {
				const change = event as EditorEvent;
				if (change.kind === EditorEventType.Change) {
					latestMarkdown.current = change.value;
					onChangeRef.current(change.value);
				}
			}, onLogMessage: () => {},
		};
		void createEditor(host.current, props, renderer, createCodeEditor).then(control => {
			if (mounted) { editor.current = control; setEditorReady(true); }
			else control.remove();
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
			</div>
			<div className="editor-host" ref={host} />
			{pasteMessage ? <p className="editor-notice" role="status">{pasteMessage}</p> : null}
		</section>
	);
}
