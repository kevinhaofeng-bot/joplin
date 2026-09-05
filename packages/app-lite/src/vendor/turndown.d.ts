declare class TurndownService {
	constructor(options?: Record<string, unknown>);
	use(plugin: unknown): this;
	remove(selector: string): this;
	turndown(input: HTMLElement): string;
}
export default TurndownService;
