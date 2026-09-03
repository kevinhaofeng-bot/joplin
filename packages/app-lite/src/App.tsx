import { useEffect, useState } from 'react';
import { getRuntimeInfo, RuntimeInfo } from './runtime';

type Props = {
	loadRuntimeInfo?: ()=> Promise<RuntimeInfo>;
};

export default function App({ loadRuntimeInfo = getRuntimeInfo }: Props) {
	const [runtime, setRuntime] = useState<RuntimeInfo | null>(null);

	useEffect(() => {
		void loadRuntimeInfo().then(setRuntime);
	}, [loadRuntimeInfo]);

	return (
		<main>
			{runtime ? <>
				<p>本地资料库已隔离</p>
				<p>{runtime.profileDirectory}</p>
			</> : '正在准备独立资料库…'}
		</main>
	);
}
