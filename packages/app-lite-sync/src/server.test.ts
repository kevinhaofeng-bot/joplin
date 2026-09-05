import { PassThrough, Readable, Writable } from 'node:stream';
import { MAX_FRAME_BYTES } from './protocol';

type Server = { runServer(input: Readable, output: Writable): Promise<void> };

function memoryOutput(): { output: Writable; lines: string[] } {
	const lines: string[] = [];
	const output = new Writable({
		write(chunk, _encoding, callback) {
			lines.push(chunk.toString());
			callback();
		},
	});
	return { output, lines };
}

const frame = (id: string, command: string, params: Record<string, unknown> = {}): string => JSON.stringify({
	id,
	protocolVersion: 1,
	command,
	params,
});

const frameWithPayloadBytes = (byteLength: number): string => {
	const value = { id: 'boundary', protocolVersion: 1, command: 'hello', params: { padding: '' } };
	const emptyLength = Buffer.byteLength(JSON.stringify(value), 'utf8');
	value.params.padding = 'x'.repeat(byteLength - emptyLength);
	return JSON.stringify(value);
};

describe('stdio sidecar server', () => {
	test('rejects invalid UTF-8 fatally without replacement or raw-byte leakage', async () => {
		const { runServer } = require('./server') as Server;
		const { output, lines } = memoryOutput();
		const valid = Buffer.from(`${frame('utf8', 'hello', { marker: 'secret-marker' })}\n`, 'utf8');
		const invalid = Buffer.from(valid);
		const idByte = valid.indexOf(Buffer.from('"utf8"', 'utf8')) + 2;
		invalid[idByte] = 0xff;

		await runServer(Readable.from([invalid]), output);

		expect(JSON.parse(lines[0])).toEqual({ id: '', ok: false, error: { code: 'INVALID_REQUEST', message: '请求格式无效' } });
		expect(lines[0]).not.toContain('\ufffd');
		expect(lines[0]).not.toContain('secret-marker');
	});

	test('completes shutdown promptly while stdin remains open for a single frame', async () => {
		const { runServer } = require('./server') as Server;
		const { output, lines } = memoryOutput();
		const input = new PassThrough();
		const running = runServer(input, output);

		input.write(`${frame('stop', 'shutdown')}\n`);
		await expect(Promise.race([
			running,
			new Promise((_, reject) => setTimeout(() => reject(new Error('shutdown waited for EOF')), 500)),
		])).resolves.toBeUndefined();

		expect(input.destroyed).toBe(true);
		expect(lines.map(line => JSON.parse(line))).toEqual([{ id: 'stop', ok: true, result: { stopped: true } }]);
	});

	test('completes shutdown promptly when hello and shutdown share an open stdin chunk', async () => {
		const { runServer } = require('./server') as Server;
		const { output, lines } = memoryOutput();
		const input = new PassThrough();
		const running = runServer(input, output);

		input.write(`${frame('hello', 'hello')}\n${frame('stop', 'shutdown')}\n`);
		await expect(Promise.race([
			running,
			new Promise((_, reject) => setTimeout(() => reject(new Error('shutdown waited for EOF')), 500)),
		])).resolves.toBeUndefined();

		expect(input.destroyed).toBe(true);
		expect(lines.map(line => JSON.parse(line))).toMatchObject([
			{ id: 'hello', ok: true },
			{ id: 'stop', ok: true, result: { stopped: true } },
		]);
	});

	test('returns promptly when an open delimiter-free input reaches the limit', async () => {
		const { runServer } = require('./server') as Server;
		const { output, lines } = memoryOutput();
		let emitted = false;
		const input = new Readable({
			read() {
				if (!emitted) {
					emitted = true;
					this.push(Buffer.alloc(MAX_FRAME_BYTES, 0x78));
				}
			},
		});

		await expect(Promise.race([
			runServer(input, output),
			new Promise((_, reject) => setTimeout(() => reject(new Error('server waited for EOF')), 500)),
		])).resolves.toBeUndefined();
		expect(JSON.parse(lines[0])).toEqual({ id: '', ok: false, error: { code: 'FRAME_TOO_LARGE', message: '协议帧过大' } });
	});

	test('rejects a CRLF frame whose raw CR and LF exceed the maximum', async () => {
		const { runServer } = require('./server') as Server;
		const { output, lines } = memoryOutput();
		const payload = Buffer.from(frameWithPayloadBytes(MAX_FRAME_BYTES - 1), 'utf8');
		await runServer(Readable.from([Buffer.concat([payload, Buffer.from('\r\n')])]), output);
		expect(JSON.parse(lines[0])).toEqual({ id: '', ok: false, error: { code: 'FRAME_TOO_LARGE', message: '协议帧过大' } });
	});

	test('rejects a valid JSON tail at EOF without LF and never dispatches it', async () => {
		const { runServer } = require('./server') as Server;
		const { output, lines } = memoryOutput();
		await runServer(Readable.from([Buffer.from(frame('tail', 'hello'), 'utf8')]), output);
		expect(JSON.parse(lines[0])).toEqual({ id: '', ok: false, error: { code: 'INVALID_REQUEST', message: '请求格式无效' } });
		expect(lines).toHaveLength(1);
	});
	test('writes one JSON response per request and stops after shutdown', async () => {
		const { runServer } = require('./server') as Server;
		const { output, lines } = memoryOutput();

		await runServer(Readable.from([
			`${frame('hello', 'hello')}\n${frame('stop', 'shutdown')}\n${frame('late', 'hello')}\n`,
		]), output);

		expect(lines).toHaveLength(2);
		expect(lines.every(line => line.endsWith('\n'))).toBe(true);
		const responses = lines.map(line => JSON.parse(line));
		expect(responses[0]).toMatchObject({ id: 'hello', ok: true, result: { protocolVersion: 1, joplinVersion: '3.7.0' } });
		expect(responses[1]).toMatchObject({ id: 'stop', ok: true });
		for (const response of responses) expect(response).not.toHaveProperty('secret');
	});

	test('returns an unassociated invalid-request response for malformed JSON', async () => {
		const { runServer } = require('./server') as Server;
		const { output, lines } = memoryOutput();

		await runServer(Readable.from(['{not-json}\n']), output);

		expect(lines).toHaveLength(1);
		expect(JSON.parse(lines[0])).toEqual({ id: '', ok: false, error: { code: 'INVALID_REQUEST', message: '请求格式无效' } });
	});

	test('rejects an oversized frame by byte length without echoing its marker', async () => {
		const { runServer } = require('./server') as Server;
		const { output, lines } = memoryOutput();
		const marker = 'OVERSIZED-SECRET-MARKER';

		await runServer(Readable.from([`${'x'.repeat(MAX_FRAME_BYTES + 1)}${marker}\n`]), output);

		expect(lines).toHaveLength(1);
		expect(JSON.parse(lines[0])).toEqual({ id: '', ok: false, error: { code: 'FRAME_TOO_LARGE', message: '协议帧过大' } });
		expect(lines[0]).not.toContain(marker);
	});

	test('counts multibyte UTF-8 frame length in bytes', async () => {
		const { runServer } = require('./server') as Server;
		const { output, lines } = memoryOutput();
		const multibyteFrame = '中'.repeat(Math.floor(MAX_FRAME_BYTES / 3) + 1);

		await runServer(Readable.from([`${multibyteFrame}\n`]), output);

		expect(JSON.parse(lines[0])).toEqual({ id: '', ok: false, error: { code: 'FRAME_TOO_LARGE', message: '协议帧过大' } });
	});

	test('replaces an oversized encode response with a bounded correlated failure', async () => {
		const { runServer } = require('./server') as Server;
		const { output, lines } = memoryOutput();
		const item = {
			id: '11111111111111111111111111111111',
			type_: 1,
			title: 'Fixture note',
			body: 'x'.repeat(MAX_FRAME_BYTES - 500),
			parent_id: '22222222222222222222222222222222',
			is_todo: 1,
			created_time: 1788566400000,
			updated_time: 1788566460000,
		};
		const line = JSON.stringify({ id: 'oversize', protocolVersion: 1, command: 'encodeItem', params: { item } });

		expect(Buffer.byteLength(line, 'utf8')).toBeLessThanOrEqual(MAX_FRAME_BYTES);
		await runServer(Readable.from([`${line}\n`]), output);

		expect(lines).toHaveLength(1);
		expect(Buffer.byteLength(lines[0], 'utf8')).toBeLessThanOrEqual(MAX_FRAME_BYTES);
		expect(JSON.parse(lines[0])).toEqual({ id: 'oversize', ok: false, error: { code: 'FRAME_TOO_LARGE', message: '协议帧过大' } });
		expect(lines[0]).not.toContain(item.body);
	});

	test('falls back to an empty id when a long-id response error would exceed the frame limit', async () => {
		const { runServer } = require('./server') as Server;
		const { output, lines } = memoryOutput();
		const marker = 'LONG-ID-MARKER-';
		const id = marker + 'i'.repeat(MAX_FRAME_BYTES - 60 - marker.length);
		const line = JSON.stringify({ id, protocolVersion: 1, command: 'hello', params: {} });

		expect(Buffer.byteLength(line, 'utf8')).toBeLessThanOrEqual(MAX_FRAME_BYTES);
		await runServer(Readable.from([`${line}\n`]), output);

		expect(lines).toHaveLength(1);
		expect(Buffer.byteLength(lines[0], 'utf8')).toBeLessThanOrEqual(MAX_FRAME_BYTES);
		expect(JSON.parse(lines[0])).toEqual({ id: '', ok: false, error: { code: 'FRAME_TOO_LARGE', message: '协议帧过大' } });
		expect(lines[0]).not.toContain(marker);
	});

	test('accepts payload of MAX minus one because its LF completes MAX bytes', async () => {
		const { runServer } = require('./server') as Server;
		const { output, lines } = memoryOutput();
		const payload = frameWithPayloadBytes(MAX_FRAME_BYTES - 1);
		expect(Buffer.byteLength(payload, 'utf8') + 1).toBe(MAX_FRAME_BYTES);

		await runServer(Readable.from([`${payload}\n`]), output);

		expect(JSON.parse(lines[0])).toMatchObject({ id: 'boundary', ok: true, result: { protocolVersion: 1 } });
	});

	test('rejects payload of MAX because its LF makes the frame oversized', async () => {
		const { runServer } = require('./server') as Server;
		const { output, lines } = memoryOutput();
		const payload = frameWithPayloadBytes(MAX_FRAME_BYTES);
		expect(Buffer.byteLength(payload, 'utf8') + 1).toBe(MAX_FRAME_BYTES + 1);

		await runServer(Readable.from([`${payload}\n`]), output);

		expect(JSON.parse(lines[0])).toEqual({ id: '', ok: false, error: { code: 'FRAME_TOO_LARGE', message: '协议帧过大' } });
	});
});
