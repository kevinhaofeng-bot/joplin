use std::{future::Future, path::PathBuf, pin::Pin, sync::Arc, time::Duration};

use serde::Serialize;
use tokio::sync::Mutex;

use crate::{profile::ProfilePaths, sync_sidecar::*};

pub const SIDECAR_UNAVAILABLE_CODE: &str = "SIDECAR_UNAVAILABLE";
pub const SIDECAR_UNAVAILABLE_MESSAGE: &str = "本地资料库不可用";
pub const SIDECAR_FAILED_CODE: &str = "SIDECAR_FAILED";
pub const SIDECAR_FAILED_MESSAGE: &str = "本地资料库操作失败";

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
}

arg_library_commands! {
    create_folder(create_folder, CreateFolderParams) -> CreateResult<Folder>,
    update_folder(update_folder, UpdateFolderParams) -> UpdateResult<Folder>,
    trash_folder(trash_folder, ExpectedUpdatedTimeParams) -> TrashResult,
    create_tag(create_tag, CreateTagParams) -> CreateResult<Tag>,
    update_tag(update_tag, UpdateTagParams) -> UpdateResult<Tag>,
    delete_tag(delete_tag, ExpectedUpdatedTimeParams) -> DeleteResult,
    list_notes(list_notes, ListNotesParams) -> NotePage,
    get_note(get_note, GetByIdParams) -> NoteDetail,
    create_note(create_note, CreateNoteParams) -> CreateResult<NoteDetail>,
    update_note(update_note, UpdateNoteParams) -> UpdateResult<NoteDetail>,
    trash_note(trash_note, ExpectedUpdatedTimeParams) -> TrashResult,
    set_note_tags(set_note_tags, SetNoteTagsParams) -> SetNoteTagsResult,
}

#[tauri::command]
pub async fn shutdown_library(state: tauri::State<'_, LibraryState>) -> Result<(), LibraryError> {
    state.shutdown().await
}
