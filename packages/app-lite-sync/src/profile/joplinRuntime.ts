import { mkdir } from 'node:fs/promises';
import { join } from 'node:path';
import { shimInit } from '../../../lib/shim-init-node';
import initLib from '../../../lib/initLib';
import JoplinDatabase from '../../../lib/JoplinDatabase';
import { DatabaseDriverNode } from '../../../lib/database-driver-node';
import shim from '../../../lib/shim';
import BaseModel from '../../../lib/BaseModel';
import BaseItem from '../../../lib/models/BaseItem';
import Setting, { AppType, Env } from '../../../lib/models/Setting';
import ItemChange from '../../../lib/models/ItemChange';
import { loadKeychainServiceAndSettings } from '../../../lib/services/SettingUtils';
import RevisionService from '../../../lib/services/RevisionService';
import BaseService from '../../../lib/services/BaseService';
import ResourceService from '../../../lib/services/ResourceService';
import ResourceFetcher from '../../../lib/services/ResourceFetcher';
import SyncTargetJoplinServer from '../../../lib/SyncTargetJoplinServer';
import SyncTargetRegistry from '../../../lib/SyncTargetRegistry';
import KvStore from '../../../lib/services/KvStore';
import EncryptionService from '../../../lib/services/e2ee/EncryptionService';
import { setRSA } from '../../../lib/services/e2ee/ppk/ppk';
import RSA from '../../../lib/services/e2ee/ppk/RSA.node';
import ShareService from '../../../lib/services/share/ShareService';
import SearchEngine from '../../../lib/services/search/SearchEngine';
import InteropService from '../../../lib/services/interop/InteropService';
import Folder from '../../../lib/models/Folder';
import Note from '../../../lib/models/Note';
import Tag from '../../../lib/models/Tag';
import Resource from '../../../lib/models/Resource';
import { reg } from '../../../lib/registry';
import Logger from '../../../utils/Logger';
import { registerItemClasses } from '../codec';
import type { ValidatedProfilePaths } from './pathPolicy';
import { createSyncSecretStore, syncConfigFromMetadata, syncError, SyncService, type SyncConfig, type SyncConfigInput, type SyncStatus, type SyncSummary, type SyncSecretStore } from './syncService';
import { JexImportService, type JexImportStatus, type JexImportSummary } from './jexImport';

const joplinVersion: string = require('../../../lib/package.json').version;

export type RuntimeHandle = Readonly<{
	schemaVersion: number;
	flush: ()=> Promise<void>;
	close: ()=> Promise<void>;
	getSyncConfig?: ()=> Promise<SyncConfig>;
	configureJoplinServer?: (input: SyncConfigInput)=> Promise<SyncConfig>;
	getSyncStatus?: ()=> Promise<SyncStatus>;
	startSync?: ()=> Promise<SyncStatus>;
	syncNow?: ()=> Promise<SyncSummary>;
	startJexImport?: (path: unknown)=> Promise<JexImportStatus>;
	getJexImportStatus?: ()=> JexImportStatus;
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
		shimInit({ nodeSqlite: require('sqlite3'), keytar: require('keytar'), appVersion: () => joplinVersion });
		const logger = new Logger();
		logger.enabled = false;
		Logger.initializeGlobalLogger(logger);
		initLib(logger);
		BaseService.logger_ = logger;
		SearchEngine.instance().setLogger(logger);

		registerItemClasses();
		initializeSettings(paths);
		await mkdir(paths.temp, { recursive: true });
		await mkdir(paths.cache, { recursive: true });

		database = new JoplinDatabase(new DatabaseDriverNode());
		database.setLogger(logger);
		await database.open({ name: paths.database });
		BaseModel.setDb(database);
		reg.setDb(database);
		SearchEngine.instance().setDb(database);
		KvStore.instance().setDb(database);
		setRSA(RSA);
		const encryptionService = EncryptionService.instance();
		BaseItem.encryptionService_ = encryptionService;
		const shareStore = {
			getState: (): { shareService: { shares: never[]; shareUsers: Record<string, never>; shareInvitations: never[]; processingShareInvitationResponse: boolean } } => ({ shareService: { shares: [], shareUsers: {}, shareInvitations: [], processingShareInvitationResponse: false } }),
			dispatch: (_action: unknown): undefined => undefined,
		} as unknown as Parameters<ShareService['initialize']>[0];
		ShareService.instance().initialize(shareStore, encryptionService);
		await loadKeychainServiceAndSettings([]);
		const clearPersistedSyncPassword = async () => {
			Setting.setValue('sync.9.password', '');
			await Setting.db().exec('DELETE FROM settings WHERE key = ?', ['sync.9.password']);
		};
		await clearPersistedSyncPassword();
		BaseItem.revisionService_ = RevisionService.instance();
		SyncTargetRegistry.addClass(SyncTargetJoplinServer);
		const syncPasswordKey = 'joplinLite.sync.9.password';
		const syncSecretStore: SyncSecretStore|null = createSyncSecretStore(
			shim.keytar?.() ?? null,
			`${Setting.value('appId')}.${syncPasswordKey}`,
			`${Setting.value('clientId')}@joplin`,
		);

		const syncService = new SyncService({
			readConfig: async () => {
				const url = Setting.value('sync.9.path');
				const username = Setting.value('sync.9.username');
				return syncConfigFromMetadata(Setting.value('sync.target'), SyncTargetJoplinServer.id(), url, username);
			},
			configure: async (input) => {
				if (!syncSecretStore) throw syncError('SYNC_AUTH_FAILED');
				const check = await SyncTargetJoplinServer.checkConfig({
					path: () => input.url,
					userContentPath: () => '',
					username: () => input.username,
					password: () => input.password,
					apiKey: () => '',
				}, SyncTargetJoplinServer.id());
				if (!check.ok) throw syncError('SYNC_AUTH_FAILED');
				const previous = {
					target: Setting.value('sync.target'), path: Setting.value('sync.9.path'), username: Setting.value('sync.9.username'),
				};
				let previousPassword: string | null = null;
				let previousPasswordRead = false;
				const clearPersistedPassword = async () => {
					const leaked = await Setting.db().selectAll<{ value: string }>('SELECT value FROM settings WHERE key = ?', ['sync.9.password']);
					if (leaked.length) await Setting.db().exec('DELETE FROM settings WHERE key = ?', ['sync.9.password']);
				};
				try {
					previousPassword = await syncSecretStore.read();
					previousPasswordRead = true;
					const saved = await syncSecretStore.write(input.password);
					if (!saved || await syncSecretStore.read() !== input.password) throw syncError('SYNC_AUTH_FAILED');
					Setting.setValue('sync.target', SyncTargetJoplinServer.id());
					Setting.setValue('sync.9.path', input.url);
					Setting.setValue('sync.9.username', input.username);
					Setting.setValue('sync.9.password', '');
					await Setting.saveAll();
					const leaked = await Setting.db().selectAll<{ value: string }>('SELECT value FROM settings WHERE key = ?', ['sync.9.password']);
					if (leaked.some(row => !!row.value)) {
						await clearPersistedPassword();
						Setting.setValue('sync.9.password', '');
						throw syncError('SYNC_AUTH_FAILED');
					}
					reg.resetSyncTarget(SyncTargetJoplinServer.id());
				} catch (error) {
					if (previousPasswordRead) {
						try {
							if (previousPassword) await syncSecretStore.write(previousPassword);
							else await syncSecretStore.remove();
						} catch {
							// Preserve the stable original error and never expose keychain details.
						}
					}
					Setting.setValue('sync.target', previous.target);
					Setting.setValue('sync.9.path', previous.path);
					Setting.setValue('sync.9.username', previous.username);
					Setting.setValue('sync.9.password', '');
					try {
						await Setting.saveAll();
						await clearPersistedPassword();
					} catch {
						try { await clearPersistedPassword(); } catch {
							// Keep the public failure fixed even if database cleanup is unavailable.
						}
					}
					throw error;
				}
			},
			syncNow: async () => {
				const config = Setting.value('sync.9.path');
				const username = Setting.value('sync.9.username');
				const password = await syncSecretStore?.read();
				if (Setting.value('sync.target') !== SyncTargetJoplinServer.id() || !config || !username || !password) throw syncError('SYNC_NOT_CONFIGURED');
				let report: { completedTime?: number; createLocal?: number; createRemote?: number; updateLocal?: number; updateRemote?: number; deleteLocal?: number; deleteRemote?: number; fetchingProcessed?: number } = {};
				const contextRaw = Setting.value('sync.9.context');
				let context: Record<string, unknown> = {};
				try { context = contextRaw ? JSON.parse(contextRaw) : {}; } catch { context = {}; }
				let nextContext: Record<string, unknown> = {};
				try {
					Setting.setValue('sync.9.password', password);
					const target = new SyncTargetJoplinServer(database);
					target.setLogger(logger);
					const synchronizer = await target.synchronizer();
					synchronizer.setEncryptionService(encryptionService);
					nextContext = await synchronizer.start({
						context,
						throwOnError: true,
						onProgress: next => { report = next; },
						saveContextHandler: next => { Setting.setValue('sync.9.context', JSON.stringify(next)); },
					});
					const fileApi = await target.fileApi();
					const fetcher = ResourceFetcher.instance();
					fetcher.setLogger(logger);
					fetcher.setFileApi(() => fileApi);
					await fetcher.fetchAll();
					await fetcher.waitForAllFinished();
				} finally {
					Setting.setValue('sync.9.password', '');
				}
				Setting.setValue('sync.9.context', JSON.stringify(nextContext));
				await ResourceService.instance().indexNoteResources();
				await Setting.saveAll();
				return {
					completedAt: report.completedTime ?? Date.now(),
					created: (report.createLocal ?? 0) + (report.createRemote ?? 0),
					updated: (report.updateLocal ?? 0) + (report.updateRemote ?? 0),
					deleted: (report.deleteLocal ?? 0) + (report.deleteRemote ?? 0),
					fetched: report.fetchingProcessed ?? 0,
				};
			},
		});
		const countItems = async () => {
			const [folders, notes, tags, resources] = await Promise.all([
				Folder.count({ where: 'deleted_time = 0' }),
				Note.count({ where: 'deleted_time = 0' }),
				Tag.count(),
				Resource.count(),
			]);
			return { folders, notes, tags, resources };
		};
		const countImported = async (before: { notes: number; folders: number; tags: number; resources: number }): Promise<JexImportSummary> => {
			const after = await countItems();
			return {
				notes: Math.max(0, after.notes - before.notes),
				folders: Math.max(0, after.folders - before.folders),
				tags: Math.max(0, after.tags - before.tags),
				resources: Math.max(0, after.resources - before.resources),
			};
		};
		const jexImportService = new JexImportService(async path => {
			const before = await countItems();
			await InteropService.instance().import({ path, format: 'jex' });
			await ItemChange.waitForAllSaved();
			await Setting.saveAll();
			return countImported(before);
		});

		const handle: RuntimeHandle = {
			schemaVersion: database.version(),
			flush: async () => {
				await ItemChange.waitForAllSaved();
				await Setting.saveAll();
			},
			close: async () => {
				await syncService.waitForIdle();
				await jexImportService.waitForIdle();
				await ItemChange.waitForAllSaved();
				await Setting.saveAll();
				await database?.close();
			},
			getSyncConfig: () => syncService.getConfig(),
			configureJoplinServer: input => syncService.configure(input),
			getSyncStatus: async () => syncService.getSyncStatus(),
			startSync: async () => syncService.startSync(),
			syncNow: () => syncService.syncNow(),
			startJexImport: path => jexImportService.start(path),
			getJexImportStatus: () => jexImportService.getStatus(),
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
