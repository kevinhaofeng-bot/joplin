export enum EditorLanguageType { Markdown = 'markdown', Html = 'html' }
export enum EditorKeymap { Default = 'default', Vim = 'vim', Emacs = 'emacs' }
export enum UserEventSource { Paste = 'input.paste', Drop = 'input.drop' }
export interface EditorControl {
	supportsCommand(name: string): boolean | Promise<boolean>;
	execCommand(name: string, ...args: unknown[]): void | Promise<unknown>;
	undo(): void; redo(): void; select(anchor: number, head: number): void;
	setScrollPercent(fraction: number): void; insertText(text: string, source?: string): void;
	updateBody(newBody: string): void; remove(): void; focus(): void;
}
export interface EditorTheme { [key: string]: unknown; themeId: number; fontFamily: string; fontSize: number; paddingBottom: number }
export interface EditorSettings {
	themeData: EditorTheme; useExternalSearch: boolean; automatchBraces: boolean; autocompleteMarkup: boolean;
	ignoreModifiers: boolean; language: EditorLanguageType; keymap: EditorKeymap; preferMacShortcuts: boolean;
	tabMovesFocus: boolean; markdownMarkEnabled: boolean; markdownInsertEnabled: boolean; katexEnabled: boolean;
	spellcheckEnabled: boolean; inlineRenderingEnabled: boolean; tableEditingEnabled: boolean; imageRenderingEnabled: boolean;
	readOnly: boolean; highlightActiveLine: boolean; indentWithTabs: boolean; editorLabel: string;
}
export interface EditorProps {
	settings: EditorSettings; initialText: string; initialNoteId: string; onLocalize: (input: string)=> string | Promise<string>;
	onPasteFile: ((data: File)=> Promise<void>) | null; onEvent: (event: unknown)=> void; onLogMessage: (message: string)=> void;
}
