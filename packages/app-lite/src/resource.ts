import type { CreateImageResourceParams, Resource } from './library';

export const MAX_PASTED_IMAGE_BYTES = 10 * 1024 * 1024;
const IMAGE_MIME_EXTENSIONS: Record<string, string> = {
	'image/png': 'png',
	'image/jpeg': 'jpg',
	'image/gif': 'gif',
	'image/webp': 'webp',
	'image/avif': 'avif',
	'image/bmp': 'bmp',
};

export function imageExtension(mime: string): string | null {
	return IMAGE_MIME_EXTENSIONS[mime.toLowerCase()] ?? null;
}

export async function imageFileToParams(file: File): Promise<CreateImageResourceParams> {
	const mime = file.type.toLowerCase();
	if (!imageExtension(mime)) throw new Error('仅支持 PNG、JPEG、GIF、WebP、AVIF 或 BMP 图片');
	if (!Number.isSafeInteger(file.size) || file.size < 0 || file.size > MAX_PASTED_IMAGE_BYTES) throw new Error('图片过大');
	const bytes = new Uint8Array(await file.arrayBuffer());
	if (bytes.length > MAX_PASTED_IMAGE_BYTES) throw new Error('图片过大');
	let binary = '';
	for (let offset = 0; offset < bytes.length; offset += 0x8000) {
		binary += String.fromCharCode(...bytes.subarray(offset, offset + 0x8000));
	}
	const extension = imageExtension(mime)!;
	return {
		title: file.name || `image.${extension}`,
		mime,
		base64: btoa(binary),
	};
}

export async function createImageFromFile(file: File, create: (params: CreateImageResourceParams)=> Promise<Resource>): Promise<Resource> {
	return create(await imageFileToParams(file));
}
