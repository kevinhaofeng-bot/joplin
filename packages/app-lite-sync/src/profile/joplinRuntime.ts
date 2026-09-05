import { mkdir } from 'node:fs/promises';
import { join } from 'node:path';
import { shimInit } from '../../../lib/shim-init-node';
import initLib from '../../../lib/initLib';
import JoplinDatabase from '../../../lib/JoplinDatabase';
import { DatabaseDriverNode } from '../../../lib/database-driver-node';
import BaseModel from '../../../lib/BaseModel';
import BaseItem from '../../../lib/models/BaseItem';
import Setting, { AppType, Env } from '../../../lib/models/Setting';
import ItemChange from '../../../lib/models/ItemChange';
import { loadKeychainServiceAndSettings } from '../../../lib/services/SettingUtils';
import RevisionService from '../../../lib/services/RevisionService';
import BaseService from '../../../lib/services/BaseService';
import { reg } from '../../../lib/registry';
import Logger from '../../../utils/Logger';
import { registerItemClasses } from '../codec';
import type { ValidatedProfilePaths } from './pathPolicy';

const joplinVersion: string = require('../../../lib/package.json').version;

export type RuntimeHandle = Readonly<{
	schemaVersion: number;
	flush: ()=> Promise<void>;
	close: ()=> Promise<void>;
}>;

function initializeSettings(paths: ValidatedProfilePaths): void {
	Setting.setConstant('appId', 'com.kevinhao.joplin-lite');
	Setting.setConstant('appName', 'Joplin Lite');
	Setting.setConstant('appType', AppType.Desktop);
	Setting.setConstant('env', Env.Prod);
	Setting.setConstant('resourceDirName', 'resources');
	Setting.setConstant('resourceDir', paths.resources);
	Setting.setConstant('profileDir', paths.root);
	Setting.setConstant('rootProfileDir', paths.root);
	Setting.setConstant('tempDir', paths.temp);
	Setting.setConstant('cacheDir', paths.cache);
	Setting.setConstant('pluginDataDir', join(paths.root, 'plugin-data'));
	Setting.setConstant('pluginDir', join(paths.root, 'plugins'));
	Setting.setConstant('homeDir', paths.root);
	Setting.setConstant('isSubProfile', false);
	Setting.autoSaveEnabled = false;
}

export async function openJoplinRuntime(paths: ValidatedProfilePaths): Promise<RuntimeHandle> {
	let database: JoplinDatabase | undefined;
	try {
		shimInit({ nodeSqlite: require('sqlite3'), appVersion: () => joplinVersion });
		const logger = new Logger();
		logger.enabled = false;
		Logger.initializeGlobalLogger(logger);
		initLib(logger);
		BaseService.logger_ = logger;

		registerItemClasses();
		initializeSettings(paths);
		await mkdir(paths.temp, { recursive: true });
		await mkdir(paths.cache, { recursive: true });

		database = new JoplinDatabase(new DatabaseDriverNode());
		database.setLogger(logger);
		await database.open({ name: paths.database });
		BaseModel.setDb(database);
		reg.setDb(database);
		await loadKeychainServiceAndSettings([]);
		BaseItem.revisionService_ = RevisionService.instance();

		const handle: RuntimeHandle = {
			schemaVersion: database.version(),
			flush: async () => {
				await ItemChange.waitForAllSaved();
				await Setting.saveAll();
			},
			close: async () => {
				await ItemChange.waitForAllSaved();
				await Setting.saveAll();
				await database?.close();
			},
		};
		return handle;
	} catch {
		try {
			await database?.close();
		} catch {
			// Deliberately keep runtime failures opaque at the protocol boundary.
		}
		throw new Error('PROFILE_OPEN_FAILED');
	}
}
