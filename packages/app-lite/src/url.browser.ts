export const fileUriToPath = (uri: string, platform = 'linux') => {
	if (typeof uri !== 'string' || uri.length <= 7 || !uri.startsWith('file://')) throw new TypeError('must pass in a file:// URI to convert to a file path');
	const rest = decodeURI(uri.substring(7));
	const firstSlash = rest.indexOf('/');
	let host = rest.substring(0, firstSlash);
	let path = rest.substring(firstSlash + 1).replace(/^(.+)\|/, '$1:');
	if (host === 'localhost') host = '';
	if (host) host = `//${host}`;
	if (!/^.+:/.test(path)) path = `/${path}`;
	return platform === 'win32' ? `${host}${path}`.replace(/\//g, '\\') : `${host}${path}`;
};

export const hasProtocol = (url: string, protocol: string | string[]) => {
	if (!url) return false;
	const protocols = typeof protocol === 'string' ? [protocol] : protocol;
	const normalized = url.toLowerCase();
	return protocols.some(item => normalized.startsWith(`${item.toLowerCase()}://`));
};
