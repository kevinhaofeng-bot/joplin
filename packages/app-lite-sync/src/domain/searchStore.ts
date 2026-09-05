import SearchEngineUtils from '../../../lib/services/search/SearchEngineUtils';
import SearchEngine from '../../../lib/services/search/SearchEngine';
import type { NoteEntity } from '../../../lib/services/database/types';
import { noteSummaryDto, type NoteSummaryDto } from './dto';
import { storageError, validationError } from './validation';

const SUMMARY_FIELDS = ['id', 'parent_id', 'title', 'is_todo', 'todo_due', 'todo_completed', 'created_time', 'updated_time', 'user_created_time', 'user_updated_time', 'deleted_time'];
const MAX_QUERY_LENGTH = 256;
const MAX_LIMIT = 100;

export type SearchNotesInput = { query: unknown; limit?: unknown };
export type SearchNoteDto = NoteSummaryDto & { bodyMatch: boolean };
export type SearchNotePage = { query: string; items: SearchNoteDto[] };

type SearchResult = { id?: string; item_id?: string; fields?: string[] };
type SearchRunner = (query: string)=> Promise<{ notes: NoteEntity[]; results: SearchResult[] }>;

function input(input: SearchNotesInput): { query: string; limit: number } {
	if (typeof input?.query !== 'string') throw validationError();
	const query = input.query.trim();
	if (!query || Array.from(query).length > MAX_QUERY_LENGTH || query.includes('\0')) throw validationError();
	const limit = input.limit === undefined ? 50 : input.limit;
	if (typeof limit !== 'number' || !Number.isSafeInteger(limit) || limit < 1 || limit > MAX_LIMIT) throw validationError();
	return { query, limit };
}

async function defaultRunner(query: string) {
	const engine = SearchEngine.instance();
	await engine.syncTables();
	return SearchEngineUtils.notesForQuery(query, false, { fields: SUMMARY_FIELDS, appendWildCards: true }, engine);
}

export class SearchStore {
	private readonly runSearch: SearchRunner;

	public constructor(runSearch: SearchRunner = defaultRunner) {
		this.runSearch = runSearch;
	}

	public async search(rawInput: SearchNotesInput): Promise<SearchNotePage> {
		const { query, limit } = input(rawInput);
		try {
			const result = await this.runSearch(query);
			const bodyMatches = new Set(result.results.filter(match => match.fields?.includes('body')).map(match => match.id || match.item_id).filter(Boolean));
			const items = result.notes
				.filter(note => !note.deleted_time && !note.is_conflict)
				.slice(0, limit)
				.map(note => ({ ...noteSummaryDto(note), bodyMatch: bodyMatches.has(note.id) }));
			return { query, items };
		} catch (error) {
			if (error && typeof error === 'object' && 'code' in error) throw error;
			throw storageError();
		}
	}
}
