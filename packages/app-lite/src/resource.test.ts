import { describe, expect, it, vi } from 'vitest';
import { createImageFromFile, imageFileToParams } from './resource';

describe('image resource input', () => {
	it('encodes a supported raster image for the Tauri command', async () => {
		const file = new File([new Uint8Array([1, 2, 3])], 'picture.png', { type: 'image/png' });
		await expect(imageFileToParams(file)).resolves.toMatchObject({ title: 'picture.png', mime: 'image/png', base64: 'AQID' });
	});

	it('rejects SVG, non-images, and oversized input before invoke', async () => {
		await expect(imageFileToParams(new File(['<svg/>'], 'x.svg', { type: 'image/svg+xml' }))).rejects.toThrow('仅支持');
		await expect(imageFileToParams(new File(['text'], 'x.txt', { type: 'text/plain' }))).rejects.toThrow('仅支持');
		const oversized = { name: 'huge.png', type: 'image/png', size: 10 * 1024 * 1024 + 1, arrayBuffer: vi.fn() } as unknown as File;
		await expect(createImageFromFile(oversized, vi.fn())).rejects.toThrow('图片过大');
	});
});
