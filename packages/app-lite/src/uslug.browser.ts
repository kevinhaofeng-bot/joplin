interface Options {
	lower?: boolean;
	spaces?: boolean;
	allowedChars?: string;
}

// The editor only needs a stable heading id. Keep the upstream slug rules in
// the browser boundary without pulling its Node-only node-emoji dependency.
export default function uslug(input: string, options: Options = {}) {
	const allowedChars = options.allowedChars || '-_~';
	const lower = typeof options.lower === 'boolean' ? options.lower : true;
	const spaces = typeof options.spaces === 'boolean' ? options.spaces : false;
	const chars = (input || '').normalize('NFKC').split('');
	const rv: string[] = [];
	let regexes: Record<string, RegExp> | undefined;
	try {
		regexes = {
			L: new RegExp('\\p{L}', 'u'), N: new RegExp('\\p{N}', 'u'),
			Z: new RegExp('\\p{Z}', 'u'), M: new RegExp('\\p{M}', 'u'),
		};
	} catch {
		regexes = undefined;
	}
	for (const c of chars) {
		const code = c.charCodeAt(0);
		if ((0x4E00 <= code && code <= 0x9FFF) || (0xAC00 <= code && code <= 0xD7A3)) {
			rv.push(c);
			continue;
		}
		if ((0x3000 <= code && code <= 0x3002) || (0xFF01 <= code && code <= 0xFF02)) rv.push(' ');
		if (allowedChars.includes(c)) {
			rv.push(c);
			continue;
		}
		const category = regexes && Object.entries(regexes).find(([, pattern]) => pattern.test(c))?.[0];
		if (category && 'LNM'.includes(category)) rv.push(c);
		if (category === 'Z') rv.push(' ');
	}
	let slug = rv.join('').replace(/^\s+|\s+$/g, '').replace(/\s+/g, ' ');
	if (!spaces) slug = slug.replace(/[\s-]+/g, '-');
	return lower ? slug.toLowerCase() : slug;
}
