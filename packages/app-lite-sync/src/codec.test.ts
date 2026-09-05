import { readFile } from 'node:fs/promises';
import { join } from 'node:path';

const fixtureDir = join(__dirname, '..', 'fixtures', 'v1');

type Codec = {
	decodeItem(raw: string): Promise<Record<string, unknown>>;
	encodeItem(item: Record<string, unknown>): Promise<string>;
	registerItemClasses(): void;
};

	describe('official item codec adapter', () => {
	test('round-trips every artificial official item fixture by manifest fields', async () => {
		const manifest = JSON.parse(await readFile(join(fixtureDir, 'manifest.json'), 'utf8')) as Record<string, string[]>;
		const { decodeItem, encodeItem, registerItemClasses } = require('./codec') as Codec;
		registerItemClasses();
		const expectedTypes: Record<string, number> = {
			'note.txt': 1,
			'folder.txt': 2,
			'resource.txt': 4,
			'tag.txt': 5,
			'note-tag.txt': 6,
			'master-key.txt': 9,
			'revision.txt': 13,
		};

		for (const [filename, fields] of Object.entries(manifest)) {
			const raw = await readFile(join(fixtureDir, filename), 'utf8');
			const decoded = await decodeItem(raw);
			expect(decoded.type_).toBe(expectedTypes[filename]);
			if (filename === 'note.txt') {
				expect(decoded).toMatchObject({
					id: '11111111111111111111111111111111',
					parent_id: '22222222222222222222222222222222',
					title: 'Fixture note',
					body: '第一行\n第二行中文\n\n![fixture](:/33333333333333333333333333333333)',
					is_todo: 1,
					created_time: 1788566400000,
					updated_time: 1788566460000,
				});
			}
			const encoded = await encodeItem(decoded);
			const roundTripped = await decodeItem(encoded);

			for (const field of fields) expect(roundTripped[field]).toEqual(decoded[field]);
		}
	});

	test('rejects a path traversal item ID with a fixed redacted error', async () => {
		const { decodeItem, registerItemClasses } = require('./codec') as Codec;
		registerItemClasses();
		const malicious = '../../escape';

		await expect(decodeItem(`id: ${malicious}\ntype_: 2`)).rejects.toMatchObject({
			code: 'INVALID_ITEM',
			message: 'Joplin 项目格式无效',
		});
		await expect(decodeItem(`id: ${malicious}\ntype_: 2`)).rejects.not.toHaveProperty('message', expect.stringContaining(malicious));
	});

	test('rejects a path traversal resource extension with a fixed redacted error', async () => {
		const { decodeItem, registerItemClasses } = require('./codec') as Codec;
		registerItemClasses();
		const malicious = '../png';

		await expect(decodeItem(`id: 33333333333333333333333333333333\nfile_extension: ${malicious}\ntype_: 4`)).rejects.toMatchObject({
			code: 'INVALID_ITEM',
			message: 'Joplin 项目格式无效',
		});
		await expect(decodeItem(`id: 33333333333333333333333333333333\nfile_extension: ${malicious}\ntype_: 4`)).rejects.not.toHaveProperty('message', expect.stringContaining(malicious));
	});
});
