import { render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import App from './App';

describe('App', () => {
	it('renders the semantic writing shell after initialization', async () => {
		render(<App loadRuntimeInfo={async () => ({
			appName: 'Joplin Lite',
			profileDirectory: '/tmp/joplin-lite-test',
		})} />);

		expect(screen.getByText('正在准备独立资料库…')).toBeInTheDocument();
		expect(await screen.findByRole('navigation', { name: '导航' })).toBeInTheDocument();
		expect(screen.getByRole('complementary', { name: '笔记列表' })).toBeInTheDocument();
		expect(screen.getByRole('main', { name: '编辑区' })).toBeInTheDocument();
		expect(await screen.findByText('本地资料库已隔离')).toBeInTheDocument();
		expect(screen.getByText('/tmp/joplin-lite-test').tagName).toBe('CODE');
		expect(screen.queryByRole('button', { name: '同步' })).not.toBeInTheDocument();
	});

	it('fails closed without implying that notes were changed', async () => {
		render(<App loadRuntimeInfo={async () => { throw new Error('bridge offline'); }} />);

		expect(await screen.findByRole('alert')).toHaveTextContent('初始化失败');
		expect(screen.getByRole('alert')).toHaveTextContent('没有修改现有笔记');
	});
});
