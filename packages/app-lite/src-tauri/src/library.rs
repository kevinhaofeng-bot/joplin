use std::{future::Future, io::Write, path::PathBuf, pin::Pin, sync::Arc, time::Duration};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use tempfile::Builder as TempFileBuilder;
use tokio::sync::Mutex;

use crate::{
    profile::{ProfilePaths, resource_path, validate_resource_file},
    sync_sidecar::*,
};

pub const SIDECAR_UNAVAILABLE_CODE: &str = "SIDECAR_UNAVAILABLE";
pub const SIDECAR_UNAVAILABLE_MESSAGE: &str = "本地资料库不可用";
pub const SIDECAR_FAILED_CODE: &str = "SIDECAR_FAILED";
pub const SIDECAR_FAILED_MESSAGE: &str = "本地资料库操作失败";
const MAX_PASTED_IMAGE_BYTES: usize = 10 * 1024 * 1024;

fn image_signature_matches(mime: &str, bytes: &[u8]) -> bool {
    match mime {
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "image/webp" => bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP",
        "image/bmp" => bytes.starts_with(b"BM"),
        "image/avif" => {
            bytes.len() >= 12
                && &bytes[4..8] == b"ftyp"
                && (&bytes[8..12] == b"avif" || &bytes[8..12] == b"avis")
        }
        _ => false,
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateImageResourceParams {
    pub title: String,
    pub mime: String,
    pub base64: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OpenResourceParams {
    pub id: String,
    #[serde(default)]
    pub file_extension: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LibraryError {
    pub code: &'static str,
    pub message: &'static str,
}

impl From<SidecarError> for LibraryError {
    fn from(error: SidecarError) -> Self {
        let (code, message) = match error.kind() {
            SidecarErrorKind::ProfileInUse => ("PROFILE_IN_USE", error.message()),
            SidecarErrorKind::ProfileLockRequired => ("PROFILE_LOCK_REQUIRED", error.message()),
            SidecarErrorKind::ProfileInvalid => ("PROFILE_INVALID", error.message()),
            SidecarErrorKind::ProfileNotOwned => ("PROFILE_NOT_OWNED", error.message()),
            SidecarErrorKind::ProfileAlreadyOpen => ("PROFILE_ALREADY_OPEN", error.message()),
            SidecarErrorKind::ProfileNotOpen => ("PROFILE_NOT_OPEN", error.message()),
            SidecarErrorKind::ProfileOpenFailed => ("PROFILE_OPEN_FAILED", error.message()),
            SidecarErrorKind::StorageError => ("STORAGE_ERROR", error.message()),
            SidecarErrorKind::NotFound => ("NOT_FOUND", error.message()),
            SidecarErrorKind::ValidationFailed => ("VALIDATION_FAILED", error.message()),
            SidecarErrorKind::Conflict => ("CONFLICT", error.message()),
            SidecarErrorKind::SyncNotConfigured => ("SYNC_NOT_CONFIGURED", error.message()),
            SidecarErrorKind::SyncAuthFailed => ("SYNC_AUTH_FAILED", error.message()),
            SidecarErrorKind::SyncNetwork => ("SYNC_NETWORK", error.message()),
            SidecarErrorKind::SyncBusy => ("SYNC_BUSY", error.message()),
            SidecarErrorKind::SyncFailed => ("SYNC_FAILED", error.message()),
            SidecarErrorKind::SpawnFailed => (SIDECAR_UNAVAILABLE_CODE, "本地兼容组件不可用"),
            _ => (SIDECAR_FAILED_CODE, SIDECAR_FAILED_MESSAGE),
        };
        Self { code, message }
    }
}

#[derive(Clone)]
pub struct LibraryState {
    profile: Option<ProfilePaths>,
    command: Option<SidecarCommand>,
    session: Arc<Mutex<Option<LibrarySession>>>,
}

struct LibrarySession {
    client: SidecarClient,
    opened: Option<OpenProfile>,
}

impl LibraryState {
    pub fn new(profile: ProfilePaths, command: SidecarCommand) -> Self {
        Self {
            profile: Some(profile),
            command: Some(command),
            session: Arc::new(Mutex::new(None)),
        }
    }

    pub fn unavailable() -> Self {
        Self {
            profile: None,
            command: None,
            session: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn open(&self) -> Result<OpenProfile, LibraryError> {
        let mut guard = self.session.lock().await;
        self.ensure_open_locked(&mut guard).await
    }

    pub async fn retry(&self) -> Result<OpenProfile, LibraryError> {
        let mut guard = self.session.lock().await;
        if let Some(mut session) = guard.take() {
            let _ = session.client.shutdown().await;
        }
        self.ensure_open_locked(&mut guard).await
    }

    pub async fn shutdown(&self) -> Result<(), LibraryError> {
        let mut guard = self.session.lock().await;
        let Some(mut session) = guard.take() else {
            return Ok(());
        };
        session.client.shutdown().await.map_err(LibraryError::from)
    }

    async fn start_locked(&self, guard: &mut Option<LibrarySession>) -> Result<(), LibraryError> {
        if guard
            .as_ref()
            .is_some_and(|session| session.client.state() == SidecarState::Ready)
        {
            return Ok(());
        }
        guard.take();
        let (Some(profile), Some(command)) = (&self.profile, &self.command) else {
            return Err(LibraryError {
                code: SIDECAR_UNAVAILABLE_CODE,
                message: SIDECAR_UNAVAILABLE_MESSAGE,
            });
        };
        #[cfg(target_os = "macos")]
        let client =
            SidecarClient::start_for_profile(command.clone(), profile, Duration::from_secs(10))
                .await
                .map_err(LibraryError::from)?;
        #[cfg(not(target_os = "macos"))]
        let client = {
            let _ = (profile, command);
            return Err(LibraryError {
                code: SIDECAR_UNAVAILABLE_CODE,
                message: SIDECAR_UNAVAILABLE_MESSAGE,
            });
        };
        *guard = Some(LibrarySession {
            client,
            opened: None,
        });
        Ok(())
    }

    async fn ensure_open_locked(
        &self,
        guard: &mut Option<LibrarySession>,
    ) -> Result<OpenProfile, LibraryError> {
        self.start_locked(guard).await?;
        let session = guard.as_mut().expect("start_locked installs a session");
        if let Some(opened) = session.opened.as_ref() {
            return Ok(opened.clone());
        }
        let opened = session
            .client
            .open_profile()
            .await
            .map_err(LibraryError::from)?;
        session.opened = Some(opened.clone());
        Ok(opened)
    }

    async fn with_client<T, F>(&self, operation: F) -> Result<T, LibraryError>
    where
        F: for<'a> FnOnce(
            &'a mut SidecarClient,
        )
            -> Pin<Box<dyn Future<Output = Result<T, SidecarError>> + Send + 'a>>,
    {
        let mut guard = self.session.lock().await;
        self.ensure_open_locked(&mut guard).await?;
        let result = operation(&mut guard.as_mut().expect("session exists").client)
            .await
            .map_err(LibraryError::from);
        if guard.as_ref().is_some_and(|client| {
            matches!(
                client.client.state(),
                SidecarState::Failed | SidecarState::Stopped
            )
        }) {
            guard.take();
        }
        result
    }

    pub async fn profile_status(&self) -> Result<ProfileStatus, LibraryError> {
        self.with_client(|client| Box::pin(client.profile_status()))
            .await
    }

    pub async fn list_folders(&self) -> Result<Vec<Folder>, LibraryError> {
        self.with_client(|client| Box::pin(client.list_folders()))
            .await
    }
    pub async fn create_folder(
        &self,
        params: CreateFolderParams,
    ) -> Result<CreateResult<Folder>, LibraryError> {
        self.with_client(|client| Box::pin(client.create_folder(params)))
            .await
    }
    pub async fn update_folder(
        &self,
        params: UpdateFolderParams,
    ) -> Result<UpdateResult<Folder>, LibraryError> {
        self.with_client(|client| Box::pin(client.update_folder(params)))
            .await
    }
    pub async fn trash_folder(
        &self,
        params: ExpectedUpdatedTimeParams,
    ) -> Result<TrashResult, LibraryError> {
        self.with_client(|client| Box::pin(client.trash_folder(params)))
            .await
    }

    pub async fn list_tags(&self) -> Result<Vec<Tag>, LibraryError> {
        self.with_client(|client| Box::pin(client.list_tags()))
            .await
    }
    pub async fn create_tag(
        &self,
        params: CreateTagParams,
    ) -> Result<CreateResult<Tag>, LibraryError> {
        self.with_client(|client| Box::pin(client.create_tag(params)))
            .await
    }
    pub async fn update_tag(
        &self,
        params: UpdateTagParams,
    ) -> Result<UpdateResult<Tag>, LibraryError> {
        self.with_client(|client| Box::pin(client.update_tag(params)))
            .await
    }
    pub async fn delete_tag(
        &self,
        params: ExpectedUpdatedTimeParams,
    ) -> Result<DeleteResult, LibraryError> {
        self.with_client(|client| Box::pin(client.delete_tag(params)))
            .await
    }

    pub async fn list_notes(&self, params: ListNotesParams) -> Result<NotePage, LibraryError> {
        self.with_client(|client| Box::pin(client.list_notes(params)))
            .await
    }
    pub async fn search_notes(
        &self,
        params: SearchNotesParams,
    ) -> Result<SearchNotePage, LibraryError> {
        if params.query.trim().is_empty()
            || params.query.chars().count() > 256
            || params.query.contains('\0')
            || params
                .limit
                .is_some_and(|limit| !(1..=100).contains(&limit))
        {
            return Err(LibraryError {
                code: "VALIDATION_FAILED",
                message: "输入内容无效",
            });
        }
        self.with_client(|client| Box::pin(client.search_notes(params)))
            .await
    }
    pub async fn get_note(&self, params: GetByIdParams) -> Result<NoteDetail, LibraryError> {
        self.with_client(|client| Box::pin(client.get_note(params)))
            .await
    }
    pub async fn create_note(
        &self,
        params: CreateNoteParams,
    ) -> Result<CreateResult<NoteDetail>, LibraryError> {
        self.with_client(|client| Box::pin(client.create_note(params)))
            .await
    }
    pub async fn update_note(
        &self,
        params: UpdateNoteParams,
    ) -> Result<UpdateResult<NoteDetail>, LibraryError> {
        self.with_client(|client| Box::pin(client.update_note(params)))
            .await
    }
    pub async fn trash_note(
        &self,
        params: ExpectedUpdatedTimeParams,
    ) -> Result<TrashResult, LibraryError> {
        self.with_client(|client| Box::pin(client.trash_note(params)))
            .await
    }
    pub async fn set_note_tags(
        &self,
        params: SetNoteTagsParams,
    ) -> Result<SetNoteTagsResult, LibraryError> {
        self.with_client(|client| Box::pin(client.set_note_tags(params)))
            .await
    }

    pub async fn create_resource_from_path(
        &self,
        params: CreateResourceFromPathParams,
    ) -> Result<Resource, LibraryError> {
        if params
            .title
            .as_deref()
            .is_some_and(|title| title.len() > 4096 || title.contains('\0'))
        {
            return Err(LibraryError {
                code: "VALIDATION_FAILED",
                message: "输入内容无效",
            });
        }
        validate_resource_file(std::path::Path::new(&params.path)).map_err(|_| LibraryError {
            code: "VALIDATION_FAILED",
            message: "输入内容无效",
        })?;
        let path = std::fs::canonicalize(&params.path).map_err(|_| LibraryError {
            code: "VALIDATION_FAILED",
            message: "输入内容无效",
        })?;
        validate_resource_file(&path).map_err(|_| LibraryError {
            code: "VALIDATION_FAILED",
            message: "输入内容无效",
        })?;
        let params = CreateResourceFromPathParams {
            path: path.to_string_lossy().into_owned(),
            title: params.title,
        };
        self.with_client(|client| Box::pin(client.create_resource_from_path(params)))
            .await
    }

    pub async fn list_note_resources(
        &self,
        params: ListNoteResourcesParams,
    ) -> Result<Vec<Resource>, LibraryError> {
        self.with_client(|client| Box::pin(client.list_note_resources(params)))
            .await
    }

    pub async fn get_sync_config(&self) -> Result<SyncConfig, LibraryError> {
        self.with_client(|client| Box::pin(client.get_sync_config()))
            .await
    }

    pub async fn configure_joplin_server(
        &self,
        params: ConfigureJoplinServerParams,
    ) -> Result<SyncConfig, LibraryError> {
        if params.url.len() > 4096
            || params.username.len() > 4096
            || params.password.len() > 4096
            || params.url.contains('\0')
            || params.username.contains('\0')
            || params.password.contains('\0')
        {
            return Err(LibraryError {
                code: "VALIDATION_FAILED",
                message: "输入内容无效",
            });
        }
        self.with_client(|client| Box::pin(client.configure_joplin_server(params)))
            .await
    }

    pub async fn sync_now(&self) -> Result<SyncSummary, LibraryError> {
        self.with_client(|client| Box::pin(client.sync_now())).await
    }

    pub async fn create_image_resource(
        &self,
        params: CreateImageResourceParams,
    ) -> Result<Resource, LibraryError> {
        let extension = match params.mime.as_str() {
            "image/png" => "png",
            "image/jpeg" => "jpg",
            "image/gif" => "gif",
            "image/webp" => "webp",
            "image/avif" => "avif",
            "image/bmp" => "bmp",
            _ => {
                return Err(LibraryError {
                    code: "VALIDATION_FAILED",
                    message: "输入内容无效",
                });
            }
        };
        if params.title.is_empty() || params.title.len() > 4096 || params.title.contains('\0') {
            return Err(LibraryError {
                code: "VALIDATION_FAILED",
                message: "输入内容无效",
            });
        }
        let max_encoded = MAX_PASTED_IMAGE_BYTES.div_ceil(3) * 4 + 4;
        if params.base64.is_empty() || params.base64.len() > max_encoded {
            return Err(LibraryError {
                code: "VALIDATION_FAILED",
                message: "输入内容无效",
            });
        }
        let bytes = STANDARD.decode(params.base64).map_err(|_| LibraryError {
            code: "VALIDATION_FAILED",
            message: "输入内容无效",
        })?;
        if bytes.is_empty()
            || bytes.len() > MAX_PASTED_IMAGE_BYTES
            || !image_signature_matches(&params.mime, &bytes)
        {
            return Err(LibraryError {
                code: "VALIDATION_FAILED",
                message: "输入内容无效",
            });
        }
        let Some(profile) = self.profile.as_ref() else {
            return Err(LibraryError {
                code: SIDECAR_UNAVAILABLE_CODE,
                message: SIDECAR_UNAVAILABLE_MESSAGE,
            });
        };
        profile.ensure().map_err(|_| LibraryError {
            code: SIDECAR_UNAVAILABLE_CODE,
            message: SIDECAR_UNAVAILABLE_MESSAGE,
        })?;
        let suffix = format!(".{extension}");
        let mut temporary = TempFileBuilder::new()
            .prefix(".joplin-lite-resource-")
            .suffix(&suffix)
            .tempfile_in(profile.root())
            .map_err(|_| LibraryError {
                code: "STORAGE_ERROR",
                message: "无法保存资料库",
            })?;
        temporary.write_all(&bytes).map_err(|_| LibraryError {
            code: "STORAGE_ERROR",
            message: "无法保存资料库",
        })?;
        temporary.flush().map_err(|_| LibraryError {
            code: "STORAGE_ERROR",
            message: "无法保存资料库",
        })?;
        self.create_resource_from_path(CreateResourceFromPathParams {
            path: temporary.path().to_string_lossy().into_owned(),
            title: Some(params.title),
        })
        .await
    }

    pub async fn open_resource(&self, params: OpenResourceParams) -> Result<(), LibraryError> {
        let Some(profile) = self.profile.as_ref() else {
            return Err(LibraryError {
                code: SIDECAR_UNAVAILABLE_CODE,
                message: SIDECAR_UNAVAILABLE_MESSAGE,
            });
        };
        let path =
            resource_path(profile, &params.id, params.file_extension.as_deref()).map_err(|_| {
                LibraryError {
                    code: "VALIDATION_FAILED",
                    message: "输入内容无效",
                }
            })?;
        #[cfg(target_os = "macos")]
        {
            let status = std::process::Command::new("/usr/bin/open")
                .arg(path)
                .status()
                .map_err(|_| LibraryError {
                    code: "STORAGE_ERROR",
                    message: "无法打开附件",
                })?;
            if !status.success() {
                return Err(LibraryError {
                    code: "STORAGE_ERROR",
                    message: "无法打开附件",
                });
            }
            Ok(())
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = path;
            Err(LibraryError {
                code: SIDECAR_UNAVAILABLE_CODE,
                message: SIDECAR_UNAVAILABLE_MESSAGE,
            })
        }
    }
}

#[cfg(debug_assertions)]
fn debug_sidecar_command() -> SidecarCommand {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    SidecarCommand {
        executable: PathBuf::from("node"),
        args: vec![
            "-r".into(),
            "./packages/app-lite-sync/node_modules/ts-node/register/transpile-only".into(),
            "packages/app-lite-sync/src/main.ts".into(),
        ],
        current_dir: repo_root,
    }
}

pub fn library_state_for_app_data(app_data: PathBuf) -> LibraryState {
    #[cfg(debug_assertions)]
    {
        let Ok(profile) = ProfilePaths::try_from_app_data(app_data) else {
            return LibraryState::unavailable();
        };
        if profile.ensure().is_err() {
            return LibraryState::unavailable();
        }
        LibraryState::new(profile, debug_sidecar_command())
    }
    #[cfg(not(debug_assertions))]
    {
        let _ = app_data;
        LibraryState::unavailable()
    }
}

macro_rules! noarg_library_commands {
    ($($name:ident($method:ident) -> $result:ty),* $(,)?) => {
        $(
            #[tauri::command]
            pub async fn $name(
                state: tauri::State<'_, LibraryState>,
            ) -> Result<$result, LibraryError> {
                state.$method().await
            }
        )*
    };
}

macro_rules! arg_library_commands {
    ($($name:ident($method:ident, $params:ty) -> $result:ty),* $(,)?) => {
        $(
            #[tauri::command]
            pub async fn $name(
                state: tauri::State<'_, LibraryState>,
                params: $params,
            ) -> Result<$result, LibraryError> {
                state.$method(params).await
            }
        )*
    };
}

noarg_library_commands! {
    open_library(open) -> OpenProfile,
    retry_library(retry) -> OpenProfile,
    profile_status(profile_status) -> ProfileStatus,
    list_folders(list_folders) -> Vec<Folder>,
    list_tags(list_tags) -> Vec<Tag>,
    get_sync_config(get_sync_config) -> SyncConfig,
    sync_now(sync_now) -> SyncSummary,
}

arg_library_commands! {
    create_folder(create_folder, CreateFolderParams) -> CreateResult<Folder>,
    update_folder(update_folder, UpdateFolderParams) -> UpdateResult<Folder>,
    trash_folder(trash_folder, ExpectedUpdatedTimeParams) -> TrashResult,
    create_tag(create_tag, CreateTagParams) -> CreateResult<Tag>,
    update_tag(update_tag, UpdateTagParams) -> UpdateResult<Tag>,
    delete_tag(delete_tag, ExpectedUpdatedTimeParams) -> DeleteResult,
    list_notes(list_notes, ListNotesParams) -> NotePage,
    search_notes(search_notes, SearchNotesParams) -> SearchNotePage,
    get_note(get_note, GetByIdParams) -> NoteDetail,
    create_note(create_note, CreateNoteParams) -> CreateResult<NoteDetail>,
    update_note(update_note, UpdateNoteParams) -> UpdateResult<NoteDetail>,
    trash_note(trash_note, ExpectedUpdatedTimeParams) -> TrashResult,
    set_note_tags(set_note_tags, SetNoteTagsParams) -> SetNoteTagsResult,
    create_resource_from_path(create_resource_from_path, CreateResourceFromPathParams) -> Resource,
    list_note_resources(list_note_resources, ListNoteResourcesParams) -> Vec<Resource>,
    configure_joplin_server(configure_joplin_server, ConfigureJoplinServerParams) -> SyncConfig,
}

#[tauri::command]
pub async fn create_image_resource(
    state: tauri::State<'_, LibraryState>,
    params: CreateImageResourceParams,
) -> Result<Resource, LibraryError> {
    state.create_image_resource(params).await
}

#[tauri::command]
pub async fn open_resource(
    state: tauri::State<'_, LibraryState>,
    params: OpenResourceParams,
) -> Result<(), LibraryError> {
    state.open_resource(params).await
}

#[tauri::command]
pub async fn shutdown_library(state: tauri::State<'_, LibraryState>) -> Result<(), LibraryError> {
    state.shutdown().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_payload_requires_matching_raster_signature() {
        assert!(image_signature_matches(
            "image/png",
            b"\x89PNG\r\n\x1a\nrest"
        ));
        assert!(!image_signature_matches("image/png", b"not a png"));
        assert!(image_signature_matches(
            "image/jpeg",
            &[0xff, 0xd8, 0xff, 0xe0]
        ));
        assert!(image_signature_matches("image/webp", b"RIFFxxxxWEBPdata"));
        assert!(!image_signature_matches("image/svg+xml", b"<svg/>"));
    }

    #[test]
    fn image_payload_encoded_size_limit_is_bounded_before_decode() {
        let max_encoded = MAX_PASTED_IMAGE_BYTES.div_ceil(3) * 4 + 4;
        assert!(max_encoded < 15 * 1024 * 1024);
        assert!(max_encoded > MAX_PASTED_IMAGE_BYTES);
    }

    #[tokio::test]
    async fn search_query_limit_counts_unicode_code_points() {
        let state = LibraryState::unavailable();
        let accepted = state
            .search_notes(SearchNotesParams {
                query: "中".repeat(256),
                limit: None,
            })
            .await
            .expect_err("the unavailable state should be reached after validation");
        assert_eq!(accepted.code, SIDECAR_UNAVAILABLE_CODE);

        let rejected = state
            .search_notes(SearchNotesParams {
                query: "中".repeat(257),
                limit: None,
            })
            .await
            .expect_err("queries above the code-point limit must be rejected");
        assert_eq!(rejected.code, "VALIDATION_FAILED");
    }
}
