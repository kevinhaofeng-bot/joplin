import { useEffect, useState } from 'react';
import { getRuntimeInfo, RuntimeInfo } from './runtime';

type Props = {
	loadRuntimeInfo?: ()=> Promise<RuntimeInfo>;
};

type Initialization =
	| { kind: 'loading' }
	| { kind: 'ready'; runtime: RuntimeInfo }
	| { kind: 'failed'; message: string };

export default function App({ loadRuntimeInfo = getRuntimeInfo }: Props) {
	const [initialization, setInitialization] = useState<Initialization>({ kind: 'loading' });

	useEffect(() => {
		let mounted = true;

		void loadRuntimeInfo().then(
			runtime => {
				if (mounted) setInitialization({ kind: 'ready', runtime });
			},
			() => {
				if (mounted) {
					setInitialization({
						kind: 'failed',
						message: '初始化失败。没有修改现有笔记或资料库。',
					});
				}
			},
		);

		return () => {
			mounted = false;
		};
	}, [loadRuntimeInfo]);

	if (initialization.kind === 'loading') {
		return <main>正在准备独立资料库…</main>;
	}

	if (initialization.kind === 'failed') {
		return <main><p role="alert">{initialization.message}</p></main>;
	}

	return (
		<main>
			<p>本地资料库已隔离</p>
			<p>{initialization.runtime.profileDirectory}</p>
		</main>
	);
}
