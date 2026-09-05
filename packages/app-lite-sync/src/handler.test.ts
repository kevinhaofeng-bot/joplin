import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { failureFrame, type RequestFrame } from './protocol';

type Handler = {
	handleRequest(request: RequestFrame): Promise<{ response: unknown; shouldExit: boolean }>;
};

const fixturePath = join(__dirname, '..', 'fixtures', 'v1', 'note.txt');
const request = (command: string, params: Record<string, unknown>, id = 'r1'): RequestFrame => ({
	id,
	protocolVersion: 1,
	command,
	params,
});

describe('sidecar command handler', () => {
	test('returns the compatibility handshake and advertised capabilities', async () => {
		const { handleRequest } = require('./handler') as Handler;

		await expect(handleRequest(request('hello', {}))).resolves.toMatchObject({
			response: { ok: true, result: { protocolVersion: 1, joplinVersion: '3.7.0', capabilities: ['decodeItem', 'encodeItem', 'shutdown'] } },
			shouldExit: false,
		});
	});

	test('decodes and encodes an official note through command parameters', async () => {
		const { handleRequest } = require('./handler') as Handler;
		const noteRaw = await readFile(fixturePath, 'utf8');

		const decoded = await handleRequest(request('decodeItem', { raw: noteRaw }));
		expect(decoded).toMatchObject({ response: { ok: true, result: { id: '11111111111111111111111111111111', type_: 1 } }, shouldExit: false });

		await expect(handleRequest(request('encodeItem', { item: (decoded.response as { result: Record<string, unknown> }).result }))).resolves.toMatchObject({
			response: { ok: true },
			shouldExit: false,
		});
	});

	test('acknowledges shutdown and marks the server for exit', async () => {
		const { handleRequest } = require('./handler') as Handler;

		await expect(handleRequest(request('shutdown', {}))).resolves.toMatchObject({ response: { ok: true }, shouldExit: true });
	});

	test('returns a stable unknown-command failure without echoing parameters', async () => {
		const { handleRequest } = require('./handler') as Handler;
		const response = await handleRequest(request('not-real', { secret: 'must-not-leak' }));

		expect(response).toEqual({
			response: failureFrame('r1', 'UNKNOWN_COMMAND', '未知命令'),
			shouldExit: false,
		});
		expect(JSON.stringify(response)).not.toContain('must-not-leak');
	});

	test('rejects decode and encode parameters as invalid requests', async () => {
		const { handleRequest } = require('./handler') as Handler;

		await expect(handleRequest(request('decodeItem', { raw: 42 }))).resolves.toEqual({
			response: failureFrame('r1', 'INVALID_REQUEST', '请求格式无效'),
			shouldExit: false,
		});
		await expect(handleRequest(request('encodeItem', { item: [] }))).resolves.toEqual({
			response: failureFrame('r1', 'INVALID_REQUEST', '请求格式无效'),
			shouldExit: false,
		});
	});
});
