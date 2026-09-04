import { invoke } from '@tauri-apps/api/core';

export type RuntimeInfo = {
	appName: string;
	profileDirectory: string;
};

const INVALID_RUNTIME_INFORMATION = 'Invalid runtime information';

const isLegacyProfilePath = (profileDirectory: string) => profileDirectory
	.split(/[\\/]+/)
	.some(component => component.toLowerCase() === 'joplin-desktop');

const isRuntimeInfo = (value: unknown): value is RuntimeInfo => {
	if (typeof value !== 'object' || value === null || Array.isArray(value)) return false;

	const { appName, profileDirectory } = value as Record<string, unknown>;
	return typeof appName === 'string'
		&& appName.length > 0
		&& typeof profileDirectory === 'string'
		&& profileDirectory.length > 0
		&& !isLegacyProfilePath(profileDirectory);
};

export const getRuntimeInfo = async (): Promise<RuntimeInfo> => {
	const runtimeInfo = await invoke<unknown>('get_runtime_info');
	if (!isRuntimeInfo(runtimeInfo)) throw new Error(INVALID_RUNTIME_INFORMATION);

	return runtimeInfo;
};
