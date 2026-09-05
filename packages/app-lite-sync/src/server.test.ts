import { Readable, Writable } from 'node:stream';
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

describe('stdio sidecar server', () => {
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
});
