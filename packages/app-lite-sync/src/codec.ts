import BaseModel from '../../lib/BaseModel';
import Database from '../../lib/database';
import BaseItem from '../../lib/models/BaseItem';
import Folder from '../../lib/models/Folder';
import MasterKey from '../../lib/models/MasterKey';
import Note from '../../lib/models/Note';
import NoteTag from '../../lib/models/NoteTag';
import Resource from '../../lib/models/Resource';
import Revision from '../../lib/models/Revision';
import Tag from '../../lib/models/Tag';
import { databaseSchema } from '../../lib/services/database/types';
import { ProtocolError } from './protocol';

const INVALID_ITEM_MESSAGE = 'Joplin 项目格式无效';

const itemClasses = [Note, Folder, Resource, Tag, NoteTag, MasterKey, Revision] as const;

// BaseItem's official text codec asks the database for each model's field
// names and types. The sidecar has no profile in this phase, so provide only
// the immutable schema metadata needed by those codec methods.
type CodecSchema = Record<string, Record<string, { type: string }>>;

const codecSchemas: CodecSchema = {
	...databaseSchema,
	master_keys: {
		id: { type: 'string' },
		created_time: { type: 'number' },
		updated_time: { type: 'number' },
		source_application: { type: 'string' },
		encryption_method: { type: 'number' },
		checksum: { type: 'string' },
		content: { type: 'string' },
	},
};

const codecDatabase = {
	tableFieldNames(tableName: string): string[] {
		const fields = codecSchemas[tableName];
		if (!fields) throw new Error(`Unknown table: ${tableName}`);
		return Object.keys(fields);
	},
	tableFields(tableName: string): { name: string; type: number }[] {
		const fields = codecSchemas[tableName];
		if (!fields) throw new Error(`Unknown table: ${tableName}`);
		return Object.entries(fields).map(([name, field]) => ({
			name,
			type: field.type === 'number' ? Database.TYPE_NUMERIC : Database.TYPE_TEXT,
		}));
	},
};

let classesRegistered = false;
let databaseConfigured = false;

export function registerItemClasses(): void {
	if (!databaseConfigured) {
		BaseModel.setDb(codecDatabase as never);
		databaseConfigured = true;
	}

	if (classesRegistered) return;
	for (const itemClass of itemClasses) BaseItem.loadClass(itemClass.name, itemClass);
	classesRegistered = true;
}

function invalidItem(): ProtocolError {
	return new ProtocolError('INVALID_ITEM', INVALID_ITEM_MESSAGE);
}

export async function decodeItem(raw: string): Promise<Record<string, unknown>> {
	registerItemClasses();
	try {
		const codecInput = raw.endsWith('\r\n') ? raw.slice(0, -2) : raw.endsWith('\n') ? raw.slice(0, -1) : raw;
		return await BaseItem.unserialize(codecInput);
	} catch {
		throw invalidItem();
	}
}

export async function encodeItem(item: Record<string, unknown>): Promise<string> {
	registerItemClasses();
	try {
		const itemClass = BaseItem.itemClass(item);
		return await itemClass.serialize(item);
	} catch {
		throw invalidItem();
	}
}
