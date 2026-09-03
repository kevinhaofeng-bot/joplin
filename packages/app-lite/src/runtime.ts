import { invoke } from '@tauri-apps/api/core';

export type RuntimeInfo = {
	appName: string;
	profileDirectory: string;
};

export const getRuntimeInfo = () => invoke<RuntimeInfo>('get_runtime_info');
