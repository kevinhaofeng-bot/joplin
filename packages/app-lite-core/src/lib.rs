pub mod document;
pub mod resource;

pub use document::{CanonicalDocument, CanonicalHtml, DocumentError, SearchText};
pub use resource::{ResourceBlob, ResourceError, ResourceId, ResourceInput, ResourceStore};
