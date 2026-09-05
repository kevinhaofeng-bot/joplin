import { decodeItem, encodeItem, registerItemClasses } from './codec';
import {
	failureFrame,
	ProtocolError,
	successFrame,
	type RequestFrame,
	type ResponseFrame,
} from './protocol';

const joplinVersion: string = require('../../lib/package.json').version;
const capabilities = ['decodeItem', 'encodeItem', 'shutdown'] as const;

function invalidRequest(): ProtocolError {
	return new ProtocolError('INVALID_REQUEST', '请求格式无效');
}

function isObject(value: unknown): value is Record<string, unknown> {
	return value !== null && typeof value === 'object' && !Array.isArray(value);
}

export async function handleRequest(request: RequestFrame): Promise<{ response: ResponseFrame; shouldExit: boolean }> {
	registerItemClasses();

	try {
		switch (request.command) {
			case 'hello':
				return {
					response: successFrame(request.id, {
						protocolVersion: 1,
						joplinVersion,
						capabilities,
					}),
					shouldExit: false,
				};
			case 'decodeItem': {
				if (typeof request.params.raw !== 'string') throw invalidRequest();
				return { response: successFrame(request.id, await decodeItem(request.params.raw)), shouldExit: false };
			}
			case 'encodeItem': {
				if (!isObject(request.params.item)) throw invalidRequest();
				return { response: successFrame(request.id, await encodeItem(request.params.item)), shouldExit: false };
			}
			case 'shutdown':
				return { response: successFrame(request.id, { stopped: true }), shouldExit: true };
			default:
				return { response: failureFrame(request.id, 'UNKNOWN_COMMAND', '未知命令'), shouldExit: false };
		}
	} catch (error) {
		if (error instanceof ProtocolError) {
			return { response: failureFrame(request.id, error.code, error.message), shouldExit: false };
		}
		return { response: failureFrame(request.id, 'INVALID_ITEM', 'Joplin 项目格式无效'), shouldExit: false };
	}
}
