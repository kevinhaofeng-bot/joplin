import { cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { htmlToMarkdown, markdownToHtml, default as RichTextEditor } from './RichTextEditor';

const { removeEditor, insertText, editorProps } = vi.hoisted(() => ({ removeEditor: vi.fn(), insertText: vi.fn(), editorProps: { current: null as any } }));
vi.mock('@joplin/editor/ProseMirror', () => ({
	createEditor: vi.fn(async (_host: unknown, props: any) => {
		editorProps.current = props;
		return {
			remove: removeEditor,
			updateBody: vi.fn(),
			execCommand: vi.fn(),
			insertText,
		};
	}),
}));

describe('RichTextEditor renderer adapter', () => {
	beforeEach(() => { vi.clearAllMocks(); editorProps.current = null; });
	afterEach(() => cleanup());
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

	it('creates and inserts a pasted image resource at the editor selection', async () => {
		const resource = { id: 'a'.repeat(32), title: 'picture.png', mime: 'image/png', fileExtension: 'png', size: 3, createdTime: 1, updatedTime: 2, markup: '![]( :/a )' };
		const onCreateImageResource = vi.fn(async () => resource);
		const onResourceCreated = vi.fn();
		render(<RichTextEditor noteId="note-1" markdown="" onChange={vi.fn()} onCreateImageResource={onCreateImageResource} onResourceCreated={onResourceCreated} />);
		await waitFor(() => expect(editorProps.current).not.toBeNull());
		await editorProps.current.onPasteFile(new File([new Uint8Array([1, 2, 3])], 'picture.png', { type: 'image/png' }));
		expect(onCreateImageResource).toHaveBeenCalledWith(expect.objectContaining({ title: 'picture.png', mime: 'image/png', base64: 'AQID' }));
		expect(insertText).toHaveBeenCalledWith(resource.markup, 'input.paste');
		expect(onResourceCreated).toHaveBeenCalledWith(resource);
	});

	it('does not insert a rejected image and treats picker cancellation as a no-op', async () => {
		const onCreateImageResource = vi.fn();
		const view = render(<RichTextEditor noteId="note-1" markdown="" onChange={vi.fn()} onCreateImageResource={onCreateImageResource} onChooseResource={vi.fn(async () => null)} />);
		await waitFor(() => expect(editorProps.current).not.toBeNull());
		await editorProps.current.onPasteFile(new File(['<svg/>'], 'bad.svg', { type: 'image/svg+xml' }));
		expect(onCreateImageResource).not.toHaveBeenCalled();
		fireEvent.click(view.getByRole('button', { name: '附件' }));
		await waitFor(() => expect(insertText).not.toHaveBeenCalled());
	});
});
