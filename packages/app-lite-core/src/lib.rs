pub mod document;
pub mod resource;

pub use document::{CanonicalDocument, CanonicalHtml, DocumentError, SearchText};
pub use resource::{
    BlobHash, ResourceBlob, ResourceError, ResourceId, ResourceInput, ResourceStore,
};
