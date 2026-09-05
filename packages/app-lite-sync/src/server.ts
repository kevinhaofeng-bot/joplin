import { createInterface } from 'node:readline';
import type { Readable, Writable } from 'node:stream';
import { handleRequest } from './handler';
import { MAX_FRAME_BYTES, parseRequestFrame, failureFrame, ProtocolError } from './protocol';

export async function runServer(input: Readable, output: Writable): Promise<void> {
	const lines = createInterface({ input, crlfDelay: Infinity });

	try {
		for await (const line of lines) {
			let response;
			let shouldExit = false;

			try {
				if (Buffer.byteLength(line, 'utf8') > MAX_FRAME_BYTES) {
					throw new ProtocolError('FRAME_TOO_LARGE', '协议帧过大');
				}
				const request = parseRequestFrame(line);
				const handled = await handleRequest(request);
				response = handled.response;
				shouldExit = handled.shouldExit;
			} catch (error) {
				if (error instanceof ProtocolError) {
					response = failureFrame('', error.code, error.message);
				} else {
					response = failureFrame('', 'INVALID_REQUEST', '请求格式无效');
				}
			}

			output.write(`${JSON.stringify(response)}\n`);
			if (shouldExit) {
				lines.close();
				break;
			}
		}
	} finally {
		lines.close();
	}
}
