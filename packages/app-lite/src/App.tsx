import { useEffect, useState } from 'react';
import { getRuntimeInfo, RuntimeInfo } from './runtime';
import './styles.css';

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
		return <main className="initialization-shell">正在准备独立资料库…</main>;
	}

	if (initialization.kind === 'failed') {
		return (
			<section className="initialization-failure" role="alert">
				<p>{initialization.message}</p>
			</section>
		);
	}

	return (
		<div className="app-shell">
			<nav className="navigation-rail" aria-label="导航">
				<p className="product-name">Joplin Lite</p>
				<p className="navigation-item" aria-current="page">全部笔记</p>
			</nav>
			<aside className="note-list" aria-label="笔记列表">
				<header className="pane-header">
					<h2>笔记</h2>
				</header>
				<p className="empty-note-list">笔记读取将在兼容层接入后启用</p>
			</aside>
			<main className="editor-pane" aria-label="编辑区">
				<p className="editor-empty">选择一篇笔记开始编辑</p>
				<footer className="profile-status" aria-label="资料库状态">
					<span className="status-dot" aria-hidden="true" />
					<span>本地资料库已隔离</span>
					<code>{initialization.runtime.profileDirectory}</code>
				</footer>
			</main>
		</div>
	);
}
