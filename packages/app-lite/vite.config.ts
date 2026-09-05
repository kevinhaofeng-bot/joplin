import { defineConfig } from 'vitest/config';
import react from '@vitejs/plugin-react';
import { fileURLToPath } from 'node:url';

const browserFocusHandler = fileURLToPath(new URL('./src/focusHandler.browser.ts', import.meta.url));
const browserUslug = fileURLToPath(new URL('./src/uslug.browser.ts', import.meta.url));
const browserTime = fileURLToPath(new URL('./src/time.browser.ts', import.meta.url));
const browserUrl = fileURLToPath(new URL('./src/url.browser.ts', import.meta.url));
const browserResourceUtils = fileURLToPath(new URL('./src/resourceUtils.browser.ts', import.meta.url));
const browserSanitizeHtml = fileURLToPath(new URL('./src/sanitizeHtml.browser.ts', import.meta.url));
const browserEvents = fileURLToPath(new URL('./node_modules/events/events.js', import.meta.url));

const browserEditorBoundaries = {
	name: 'app-lite-browser-editor-boundaries',
	enforce: 'pre' as const,
	resolveId(source: string, importer?: string) {
		if (source === '../../utils/sanitizeHtml' && importer?.endsWith('/packages/editor/ProseMirror/plugins/joplinEditablePlugin/joplinEditablePlugin.ts')) {
			return browserSanitizeHtml;
		}
		return null;
	},
	transform(code: string, id: string) {
		if (!id.includes('/packages/editor/ProseMirror/vendor/icons/')) return null;
		const asset = code.match(/require\('\.\/(.+?\.svg)'\)/)?.[1];
		if (!asset) return null;
		const replacement = `({ default: () => { const image = document.createElement('img'); image.src = new URL('./${asset}', import.meta.url).href; return image; } })`;
		return { code: code.replace(/require\('\.\/.+?\.svg'\)/, replacement), map: null };
	},
};

export default defineConfig({
	plugins: [browserEditorBoundaries, react()],
	resolve: {
		// The editor imports this workspace CJS utility by named export. Keep the
		// browser boundary narrow and independent of the Node-oriented Logger.
		alias: {
			events: browserEvents,
			'@joplin/lib/utils/focusHandler': browserFocusHandler,
			'@joplin/fork-uslug/lib/uslug': browserUslug,
			'@joplin/utils/time': browserTime,
			'@joplin/utils/url': browserUrl,
			'@joplin/lib/models/utils/resourceUtils': browserResourceUtils,
		},
	},
	optimizeDeps: {
		include: ['@joplin/renderer', '@joplin/turndown', '@joplin/turndown-plugin-gfm'],
	},
	server: {
		host: '127.0.0.1',
		port: 1420,
		strictPort: true,
	},
	test: {
		environment: 'jsdom',
		setupFiles: './src/test/setup.ts',
	},
});
