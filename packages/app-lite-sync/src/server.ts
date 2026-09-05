import { once } from 'node:events';
import type { Readable, Writable } from 'node:stream';
import { TextDecoder } from 'node:util';
import { handleRequest } from './handler';
import { MAX_FRAME_BYTES, parseRequestFrame, failureFrame, ProtocolError, type ResponseFrame } from './protocol';

function frameByteLength(serialized: string): number {
	return Buffer.byteLength(`${serialized}\n`, 'utf8');
}

export async function runServer(input: Readable, output: Writable): Promise<void> {
	const decoder = new TextDecoder('utf-8', { fatal: true });
	let pending = Buffer.alloc(0);
	let stopping = false;

	const writeResponse = async (response: ResponseFrame, fallbackId = ''): Promise<void> => {
		let serialized = JSON.stringify(response);
		if (frameByteLength(serialized) > MAX_FRAME_BYTES) {
			response = failureFrame(fallbackId, 'FRAME_TOO_LARGE', '协议帧过大');
			serialized = JSON.stringify(response);
			if (frameByteLength(serialized) > MAX_FRAME_BYTES) {
				response = failureFrame('', 'FRAME_TOO_LARGE', '协议帧过大');
				serialized = JSON.stringify(response);
			}
		}
		if (frameByteLength(serialized) > MAX_FRAME_BYTES) return;
		if (!output.write(`${serialized}\n`)) await once(output, 'drain');
	};

	const handleFrame = async (frame: Buffer): Promise<void> => {
		let requestId = '';
		let response;
		try {
			if (frame.length > MAX_FRAME_BYTES) throw new ProtocolError('FRAME_TOO_LARGE', '协议帧过大');
			let payload = frame.subarray(0, frame.length - 1);
			if (payload[payload.length - 1] === 0x0d) payload = payload.subarray(0, payload.length - 1);
			const line = decoder.decode(payload);
			const request = parseRequestFrame(line);
			requestId = request.id;
			const handled = await handleRequest(request);
			response = handled.response;
			stopping = handled.shouldExit;
		} catch (error) {
			if (error instanceof ProtocolError) response = failureFrame(requestId, error.code, error.message);
			else response = failureFrame(requestId, 'INVALID_REQUEST', '请求格式无效');
		}
		await writeResponse(response, requestId);
	};

	for await (const chunk of input) {
		if (stopping) break;
		let source = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
		if (pending.length) {
			const lf = source.indexOf(0x0a);
			if (lf < 0) {
				if (pending.length + source.length >= MAX_FRAME_BYTES) {
					await writeResponse(failureFrame('', 'FRAME_TOO_LARGE', '协议帧过大'));
					input.destroy();
					return;
				}
				pending = Buffer.concat([pending, source]);
				continue;
			}
			if (pending.length + lf + 1 > MAX_FRAME_BYTES) {
				await writeResponse(failureFrame('', 'FRAME_TOO_LARGE', '协议帧过大'));
				input.destroy();
				return;
			}
			await handleFrame(Buffer.concat([pending, source.subarray(0, lf + 1)]));
			pending = Buffer.alloc(0);
			source = source.subarray(lf + 1);
			if (stopping) {
				input.destroy();
				return;
			}
		}
		while (!stopping && source.length) {
			const lf = source.indexOf(0x0a);
			if (lf < 0) {
				if (source.length >= MAX_FRAME_BYTES) {
					await writeResponse(failureFrame('', 'FRAME_TOO_LARGE', '协议帧过大'));
					input.destroy();
					return;
				}
				pending = source;
				break;
			}
			await handleFrame(source.subarray(0, lf + 1));
			source = source.subarray(lf + 1);
		}
		if (stopping) {
			input.destroy();
			return;
		}
	}
	if (!stopping && pending.length) await writeResponse(failureFrame('', 'INVALID_REQUEST', '请求格式无效'));
}
