import type { EditorControl, EditorProps } from './editor-types';
export interface RendererControl {
	renderMarkupToHtml: (markup: string, options: { forceMarkdown?: boolean; isFullPageRender?: boolean })=> Promise<{ html: string; cssStrings: string[]; pluginAssets: unknown[] }>;
	renderHtmlToMarkup: (html: HTMLElement)=> string;
}
export type OnCreateCodeEditor = (parent: HTMLElement, language: unknown, onChange: (value: string)=> void)=> {
	focus(): void; remove(): void; updateBody(value: string): void; select(from: number, to: number): void;
};
export type { EditorControl, EditorProps };
