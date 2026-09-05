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
			let requestId = '';

			try {
				if (Buffer.byteLength(line, 'utf8') > MAX_FRAME_BYTES) {
					throw new ProtocolError('FRAME_TOO_LARGE', '协议帧过大');
				}
				const request = parseRequestFrame(line);
				requestId = request.id;
				const handled = await handleRequest(request);
				response = handled.response;
				shouldExit = handled.shouldExit;
			} catch (error) {
				if (error instanceof ProtocolError) {
					response = failureFrame(requestId, error.code, error.message);
				} else {
					response = failureFrame(requestId, 'INVALID_REQUEST', '请求格式无效');
				}
			}

			let serialized = JSON.stringify(response);
			if (Buffer.byteLength(serialized, 'utf8') > MAX_FRAME_BYTES) {
				response = failureFrame(requestId, 'FRAME_TOO_LARGE', '协议帧过大');
				serialized = JSON.stringify(response);
			}
			output.write(`${serialized}\n`);
			if (shouldExit) {
				lines.close();
				break;
			}
		}
	} finally {
		lines.close();
	}
}
