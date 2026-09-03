export type RuntimeInfo = {
	appName: string;
	profileDirectory: string;
};

export async function getRuntimeInfo(): Promise<RuntimeInfo> {
	throw new Error('Tauri bridge is not connected');
}
