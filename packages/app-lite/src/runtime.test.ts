import { invoke } from '@tauri-apps/api/core';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { getRuntimeInfo } from './runtime';

vi.mock('@tauri-apps/api/core', () => ({
	invoke: vi.fn(),
}));

describe('getRuntimeInfo', () => {
	beforeEach(() => {
		vi.mocked(invoke).mockReset();
	});

	it('invokes the get_runtime_info Tauri command', async () => {
		const runtimeInfo = {
			appName: 'Joplin Lite',
			profileDirectory: '/tmp/joplin-lite-test',
		};
		vi.mocked(invoke).mockResolvedValue(runtimeInfo);

		expect(await getRuntimeInfo()).toBe(runtimeInfo);

		expect(invoke).toHaveBeenCalledExactlyOnceWith('get_runtime_info');
	});

	it.each([
		{ appName: 'Joplin Lite' },
		{ profileDirectory: '/tmp/joplin-lite-test' },
	])('rejects a payload with a missing required field: %o', async payload => {
		vi.mocked(invoke).mockResolvedValue(payload);

		await expect(getRuntimeInfo()).rejects.toThrow('Invalid runtime information');
	});

	it.each([
		{ appName: '', profileDirectory: '/tmp/joplin-lite-test' },
		{ appName: 'Joplin Lite', profileDirectory: '' },
	])('rejects a payload with an empty required field: %o', async payload => {
		vi.mocked(invoke).mockResolvedValue(payload);

		await expect(getRuntimeInfo()).rejects.toThrow('Invalid runtime information');
	});

	it.each([
		'/tmp/joplin-desktop',
		'/tmp/JOPLIN-DESKTOP/profile',
		'C:\\Users\\kevin\\Joplin-Desktop\\profile',
	])('rejects a legacy Joplin profile path: %s', async profileDirectory => {
		vi.mocked(invoke).mockResolvedValue({
			appName: 'Joplin Lite',
			profileDirectory,
		});

		await expect(getRuntimeInfo()).rejects.toThrow('Invalid runtime information');
	});
});
