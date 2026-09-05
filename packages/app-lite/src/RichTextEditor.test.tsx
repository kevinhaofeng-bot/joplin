import { render, waitFor } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { htmlToMarkdown, markdownToHtml, default as RichTextEditor } from './RichTextEditor';

const { removeEditor } = vi.hoisted(() => ({ removeEditor: vi.fn() }));
vi.mock('@joplin/editor/ProseMirror', () => ({
	createEditor: vi.fn(async () => ({
		remove: removeEditor,
		updateBody: vi.fn(),
		execCommand: vi.fn(),
	})),
}));

describe('RichTextEditor renderer adapter', () => {
	it('round-trips markdown through the official renderer conversion boundary', async () => {
		const html = await markdownToHtml('## 标题\n\n- **一项**');
		const document = new DOMParser().parseFromString(html, 'text/html');
		expect(document.querySelector('h2')?.textContent).toBe('标题');
		expect(htmlToMarkdown(html)).toContain('## 标题');
		expect(htmlToMarkdown(html)).toContain('**一项**');
	});

	it('removes the official editor control when the component unmounts', async () => {
		const view = render(<RichTextEditor noteId="note-1" markdown="" onChange={vi.fn()} />);
		await waitFor(() => expect(removeEditor).not.toHaveBeenCalled());
		view.unmount();
		await waitFor(() => expect(removeEditor).toHaveBeenCalledOnce());
	});
});
