use super::LibraryError;
use crate::{LibraryRoute, NotebookId, StackId, TagId};
use serde::{Deserialize, Serialize};

const MAX_LOCATION_BYTES: usize = 16 * 1024;
const MAX_QUERY_BYTES: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LibraryShellLocation {
    Browse(LibraryRoute),
    Search(String),
}

impl Default for LibraryShellLocation {
    fn default() -> Self {
        Self::Browse(LibraryRoute::AllNotes)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredLocation {
    version: u8,
    destination: StoredDestination,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case", deny_unknown_fields)]
enum StoredDestination {
    AllNotes,
    Notebook(String),
    Stack(String),
    Tags(Vec<String>),
    Trash,
    Search(String),
}

impl LibraryShellLocation {
    pub(super) fn validate(&self) -> Result<(), LibraryError> {
        match self {
            Self::Browse(LibraryRoute::Tags(ids)) if ids.is_empty() => {
                return Err(LibraryError::InvalidLibraryShellState);
            }
            Self::Search(query) if query.trim().is_empty() || query.len() > MAX_QUERY_BYTES => {
                return Err(LibraryError::InvalidLibraryShellState);
            }
            _ => {}
        }
        self.setting_value().map(|_| ())
    }

    pub(super) fn setting_value(&self) -> Result<String, LibraryError> {
        let destination = match self {
            Self::Browse(LibraryRoute::AllNotes) => StoredDestination::AllNotes,
            Self::Browse(LibraryRoute::Notebook(id)) => StoredDestination::Notebook(id.as_str().into()),
            Self::Browse(LibraryRoute::Stack(id)) => StoredDestination::Stack(id.as_str().into()),
            Self::Browse(LibraryRoute::Tags(ids)) => {
                StoredDestination::Tags(ids.iter().map(|id| id.as_str().into()).collect())
            }
            Self::Browse(LibraryRoute::Trash) => StoredDestination::Trash,
            Self::Search(query) => StoredDestination::Search(query.clone()),
        };
        let value = serde_json::to_string(&StoredLocation { version: 1, destination })
            .map_err(|_| LibraryError::InvalidLibraryShellState)?;
        if value.len() > MAX_LOCATION_BYTES {
            return Err(LibraryError::InvalidLibraryShellState);
        }
        Ok(value)
    }

    pub(super) fn from_setting(value: &str) -> Option<Self> {
        if value.len() > MAX_LOCATION_BYTES {
            return None;
        }
        let stored: StoredLocation = serde_json::from_str(value).ok()?;
        if stored.version != 1 {
            return None;
        }
        let location = match stored.destination {
            StoredDestination::AllNotes => Self::default(),
            StoredDestination::Notebook(id) => Self::Browse(LibraryRoute::Notebook(NotebookId::parse(&id).ok()?)),
            StoredDestination::Stack(id) => Self::Browse(LibraryRoute::Stack(StackId::parse(&id).ok()?)),
            StoredDestination::Tags(ids) => Self::Browse(LibraryRoute::tags(
                ids.iter().map(|id| TagId::parse(id)).collect::<Result<Vec<_>, _>>().ok()?,
            ).ok()?),
            StoredDestination::Trash => Self::Browse(LibraryRoute::Trash),
            StoredDestination::Search(query) => Self::Search(query),
        };
        location.validate().ok()?;
        Some(location)
    }
}
