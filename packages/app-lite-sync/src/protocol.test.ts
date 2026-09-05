import {
	MAX_FRAME_BYTES,
	failureFrame,
	parseRequestFrame,
} from './protocol';

describe('parseRequestFrame', () => {
	test('accepts a valid hello request frame', () => {
		expect(parseRequestFrame('{"id":"r1","protocolVersion":1,"command":"hello","params":{}}')).toEqual({
			id: 'r1', protocolVersion: 1, command: 'hello', params: {},
		});
	});

	test('rejects malformed JSON as an invalid request', () => {
		expect(() => parseRequestFrame('{')).toThrow(expect.objectContaining({ code: 'INVALID_REQUEST' }));
	});

	test('rejects unsupported protocol versions', () => {
		expect(() => parseRequestFrame('{"id":"r1","protocolVersion":2,"command":"hello","params":{}}'))
			.toThrow(expect.objectContaining({ code: 'PROTOCOL_MISMATCH' }));
	});

	test('rejects an empty request id', () => {
		expect(() => parseRequestFrame('{"id":"","protocolVersion":1,"command":"hello","params":{}}'))
			.toThrow(expect.objectContaining({ code: 'INVALID_REQUEST' }));
	});

	test('rejects frames larger than the maximum UTF-8 byte length', () => {
		expect(() => parseRequestFrame('x'.repeat(MAX_FRAME_BYTES + 1)))
			.toThrow(expect.objectContaining({ code: 'FRAME_TOO_LARGE' }));
	});
});

test('failure frames do not expose an Error stack', () => {
	expect(failureFrame('r1', 'INVALID_REQUEST', '请求格式无效')).not.toHaveProperty('stack');
});
