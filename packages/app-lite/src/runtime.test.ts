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
		vi.mocked(invoke).mockResolvedValue({
			appName: 'Joplin Lite',
			profileDirectory: '/tmp/joplin-lite-test',
		});

		await getRuntimeInfo();

		expect(invoke).toHaveBeenCalledExactlyOnceWith('get_runtime_info');
	});
});
