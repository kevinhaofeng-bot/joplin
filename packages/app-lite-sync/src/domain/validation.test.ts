import { normalizeFolderTitle, normalizeTagTitle, validateId, validateTimestamp } from './validation';

describe('domain validation', () => {
	test.each([
		['0123456789abcdef0123456789abcdef', true],
		['ABCDEF0123456789ABCDEF0123456789', false],
		['short', false],
		['0123456789abcdef0123456789abcde!', false],
	])('validates canonical ids: %s', (value, valid) => {
		if (valid) expect(validateId(value)).toBe(value);
		else expect(() => validateId(value)).toThrow(expect.objectContaining({ code: 'VALIDATION_FAILED' }));
	});

	test('normalizes folder title using official leading slash rules', () => {
		expect(normalizeFolderTitle('/\\/ Projects')).toBe(' Projects');
		expect(() => normalizeFolderTitle('///')).toThrow(expect.objectContaining({ code: 'VALIDATION_FAILED' }));
	});

	test('normalizes tag title by trimming and NFC composition', () => {
		expect(normalizeTagTitle('  Cafe\u0301  ')).toBe('Café');
		expect(() => normalizeTagTitle('  ')).toThrow(expect.objectContaining({ code: 'VALIDATION_FAILED' }));
	});

	test('accepts only non-negative safe integer timestamps', () => {
		expect(validateTimestamp(0)).toBe(0);
		expect(validateTimestamp(Number.MAX_SAFE_INTEGER)).toBe(Number.MAX_SAFE_INTEGER);
		expect(() => validateTimestamp(-1)).toThrow(expect.objectContaining({ code: 'VALIDATION_FAILED' }));
		expect(() => validateTimestamp(Number.MAX_SAFE_INTEGER + 1)).toThrow(expect.objectContaining({ code: 'VALIDATION_FAILED' }));
	});
});
