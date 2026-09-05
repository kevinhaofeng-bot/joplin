export enum MarkupLanguage { Markdown = 1, Html = 2, Any = 3 }
export class MarkupToHtml {
	static MARKUP_LANGUAGE_MARKDOWN: number;
	static MARKUP_LANGUAGE_HTML: number;
	constructor(options?: { isSafeMode?: boolean });
	render(markupLanguage: MarkupLanguage, markup: string, theme: Record<string, unknown>, options: Record<string, unknown>): Promise<{ html: string; cssStrings: string[]; pluginAssets: unknown[] }>;
}
