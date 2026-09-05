import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { failureFrame, type RequestFrame } from './protocol';
import { createHandler, type RequestHandler } from './handler';

function codecHandler(): RequestHandler {
	return createHandler({
		status: () => ({ state: 'closed', formatVersion: 1 }),
		open: async () => ({ state: 'open', schemaVersion: 0, formatVersion: 1 }),
		flush: async (): Promise<void> => undefined,
		close: async (): Promise<void> => undefined,
	});
}

const fixturePath = join(__dirname, '..', 'fixtures', 'v1', 'note.txt');
const request = (command: string, params: Record<string, unknown>, id = 'r1'): RequestFrame => ({
	id,
	protocolVersion: 1,
	command,
	params,
});

describe('sidecar command handler', () => {
	test('returns the compatibility handshake and advertised capabilities', async () => {
		const { handleRequest } = codecHandler();

		await expect(handleRequest(request('hello', {}))).resolves.toMatchObject({
			response: { ok: true, result: { protocolVersion: 1, joplinVersion: '3.7.0', capabilities: ['decodeItem', 'encodeItem', 'shutdown', 'profileStatus', 'openProfile'] } },
			shouldExit: false,
		});
	});

	test('decodes and encodes an official note through command parameters', async () => {
		const { handleRequest } = codecHandler();
		const noteRaw = await readFile(fixturePath, 'utf8');

		const decoded = await handleRequest(request('decodeItem', { raw: noteRaw }));
		expect(decoded).toMatchObject({ response: { ok: true, result: { id: '11111111111111111111111111111111', type_: 1 } }, shouldExit: false });

		await expect(handleRequest(request('encodeItem', { item: (decoded.response as { result: Record<string, unknown> }).result }))).resolves.toMatchObject({
			response: { ok: true },
			shouldExit: false,
		});
	});

	test('acknowledges shutdown and marks the server for exit', async () => {
		const { handleRequest } = codecHandler();

		await expect(handleRequest(request('shutdown', {}))).resolves.toMatchObject({ response: { ok: true }, shouldExit: true });
	});

	test('returns a stable unknown-command failure without echoing parameters', async () => {
		const { handleRequest } = codecHandler();
		const response = await handleRequest(request('not-real', { secret: 'must-not-leak' }));

		expect(response).toEqual({
			response: failureFrame('r1', 'UNKNOWN_COMMAND', '未知命令'),
			shouldExit: false,
		});
		expect(JSON.stringify(response)).not.toContain('must-not-leak');
	});

	test('rejects decode and encode parameters as invalid requests', async () => {
		const { handleRequest } = codecHandler();

		await expect(handleRequest(request('decodeItem', { raw: 42 }))).resolves.toEqual({
			response: failureFrame('r1', 'INVALID_REQUEST', '请求格式无效'),
			shouldExit: false,
		});
		await expect(handleRequest(request('encodeItem', { item: [] }))).resolves.toEqual({
			response: failureFrame('r1', 'INVALID_REQUEST', '请求格式无效'),
			shouldExit: false,
		});
	});

	test('uses one injected session for profile status, open, and shutdown', async () => {
		const session = {
			status: jest.fn(() => ({ state: 'closed' as const, formatVersion: 1 as const })),
			open: jest.fn(async () => ({ state: 'open' as const, schemaVersion: 34, formatVersion: 1 as const })),
			flush: jest.fn(async (): Promise<void> => undefined),
			close: jest.fn(async (): Promise<void> => undefined),
		};
		const handler = createHandler(session);

		await expect(handler.handleRequest(request('profileStatus', {}))).resolves.toMatchObject({ response: { ok: true, result: { state: 'closed' } } });
		await expect(handler.handleRequest(request('openProfile', { profilePath: '/private/path-marker' }))).resolves.toMatchObject({ response: { ok: true, result: { schemaVersion: 34 } } });
		await expect(handler.handleRequest(request('shutdown', {}))).resolves.toMatchObject({ response: { ok: true, result: { stopped: true } }, shouldExit: true });
		expect(session.open).toHaveBeenCalledWith('/private/path-marker');
		expect(session.close).toHaveBeenCalledTimes(1);
	});

	test('rejects extra profile fields without echoing the path', async () => {
		const session = { status: () => ({ state: 'closed' as const, formatVersion: 1 as const }), open: jest.fn(), flush: async (): Promise<void> => undefined, close: async (): Promise<void> => undefined };
		const response = await createHandler(session).handleRequest(request('openProfile', { profilePath: '/secret/path', extra: 'secret-marker' }));
		expect(response).toEqual({ response: failureFrame('r1', 'INVALID_REQUEST', '请求格式无效'), shouldExit: false });
		expect(JSON.stringify(response)).not.toContain('secret-marker');
	});
});
