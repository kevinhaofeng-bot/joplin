use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProfileState {
    Closed,
    Open,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileStatus {
    pub state: ProfileState,
    pub format_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OpenProfile {
    pub state: ProfileState,
    pub schema_version: u64,
    pub format_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Folder {
    pub id: String,
    pub parent_id: String,
    pub title: String,
    pub created_time: u64,
    pub updated_time: u64,
    pub deleted_time: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Tag {
    pub id: String,
    pub title: String,
    pub created_time: u64,
    pub updated_time: u64,
    pub note_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MarkupLanguage {
    Markdown,
    Html,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NoteSummary {
    pub id: String,
    pub parent_id: String,
    pub title: String,
    pub is_todo: bool,
    pub todo_due: u64,
    pub todo_completed: u64,
    pub created_time: u64,
    pub updated_time: u64,
    pub user_created_time: u64,
    pub user_updated_time: u64,
    pub deleted_time: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NoteDetail {
    pub id: String,
    pub parent_id: String,
    pub title: String,
    pub is_todo: bool,
    pub todo_due: u64,
    pub todo_completed: u64,
    pub created_time: u64,
    pub updated_time: u64,
    pub user_created_time: u64,
    pub user_updated_time: u64,
    pub deleted_time: u64,
    pub body: String,
    pub markup_language: MarkupLanguage,
    pub tag_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NotePage {
    pub items: Vec<NoteSummary>,
    pub page: u64,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EmptyParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfilePathParams {
    pub profile_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartJexImportParams {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JexImportSummary {
    pub notes: u64,
    pub folders: u64,
    pub tags: u64,
    pub resources: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JexImportState {
    Idle,
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum JexImportCode {
    ImportInvalid,
    ImportBusy,
    ImportFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JexImportStatus {
    pub state: JexImportState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<JexImportSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<JexImportCode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListNotesParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GetByIdParams {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExpectedUpdatedTimeParams {
    pub id: String,
    pub expected_updated_time: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateFolderParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub parent_id: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateFolderParams {
    pub id: String,
    pub expected_updated_time: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateTagParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateTagParams {
    pub id: String,
    pub expected_updated_time: u64,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateNoteParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub parent_id: String,
    pub title: String,
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_todo: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub todo_due: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateNoteParams {
    pub id: String,
    pub expected_updated_time: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_todo: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub todo_due: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub todo_completed: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetNoteTagsParams {
    pub note_id: String,
    pub expected_updated_time: u64,
    pub tag_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateResult<T> {
    pub item: T,
    pub created: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateResult<T> {
    pub item: T,
    pub changed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TrashResult {
    pub id: String,
    pub deleted_time: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetNoteTagsResult {
    pub note_id: String,
    pub tag_ids: Vec<String>,
    pub updated_time: u64,
    pub changed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeleteResult {
    pub id: String,
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Resource {
    pub id: String,
    pub title: String,
    pub mime: String,
    pub file_extension: String,
    pub size: u64,
    pub created_time: u64,
    pub updated_time: u64,
    pub markup: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateResourceFromPathParams {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListNoteResourcesParams {
    pub note_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShutdownResult {
    pub stopped: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SyncConfig {
    pub configured: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigureJoplinServerParams {
    pub url: String,
    pub username: String,
    pub password: String,
}

impl fmt::Debug for ConfigureJoplinServerParams {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConfigureJoplinServerParams")
            .field("url", &self.url)
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SyncSummary {
    pub completed_at: u64,
    pub created: u64,
    pub updated: u64,
    pub deleted: u64,
    pub fetched: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SyncStatusState {
    Idle,
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SyncStatusCode {
    SyncNotConfigured,
    SyncAuthFailed,
    SyncNetwork,
    SyncFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SyncStatus {
    pub state: SyncStatusState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<SyncSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<SyncStatusCode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchNotesParams {
    pub query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchNote {
    pub id: String,
    pub parent_id: String,
    pub title: String,
    pub is_todo: bool,
    pub todo_due: u64,
    pub todo_completed: u64,
    pub created_time: u64,
    pub updated_time: u64,
    pub user_created_time: u64,
    pub user_updated_time: u64,
    pub deleted_time: u64,
    pub body_match: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchNotePage {
    pub query: String,
    pub items: Vec<SearchNote>,
}

#[cfg(test)]
mod tests {
    use super::{ConfigureJoplinServerParams, JexImportStatus, SearchNotesParams, SyncStatus};

    #[test]
    fn configure_debug_redacts_password() {
        let value = ConfigureJoplinServerParams {
            url: "https://example.test".into(),
            username: "user@example.test".into(),
            password: "secret-marker".into(),
        };
        let debug = format!("{value:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("secret-marker"));
    }

    #[test]
    fn search_params_reject_unknown_fields() {
        let value = serde_json::json!({ "query": "body", "unexpected": true });
        assert!(serde_json::from_value::<SearchNotesParams>(value).is_err());
    }

    #[test]
    fn sync_status_rejects_unknown_fields() {
        let value = serde_json::json!({ "state": "running", "unexpected": true });
        assert!(serde_json::from_value::<SyncStatus>(value).is_err());
    }

    #[test]
    fn sync_status_rejects_unknown_error_codes() {
        let value = serde_json::json!({ "state": "failed", "code": "SECRET_ERROR" });
        assert!(serde_json::from_value::<SyncStatus>(value).is_err());
    }

    #[test]
    fn jex_status_is_strict_and_safe() {
        let value = serde_json::json!({ "state": "succeeded", "summary": { "notes": 1, "folders": 1, "tags": 0, "resources": 1 }, "unexpected": true });
        assert!(serde_json::from_value::<JexImportStatus>(value).is_err());
    }
}
