type MaybeFocusable = {
	focus?: (...args: unknown[])=> void;
	blur?: (...args: unknown[])=> void;
};

export const focus = (_source: string, element: unknown, options: unknown = null) => {
	if (!element) return;
	const focusable = element as MaybeFocusable;
	if (typeof focusable.focus !== 'function') return;
	if (options) focusable.focus.call(element, options);
	else focusable.focus.call(element);
};

export const blur = (_source: string, element: unknown) => {
	if (!element) return;
	const focusable = element as MaybeFocusable;
	if (typeof focusable.blur === 'function') focusable.blur.call(element);
};
