#![no_std]

//! High-performance, zero-copy, streaming XML syntax reader.
//!
//! This crate tokenizes well-formed XML into fine-grained events (start tags,
//! attributes, text, comments, etc.) delivered through a [`Visitor`] trait.
//! It does not validate that xml or attribute names are legal, build a tree, resolve namespaces,
//! or expand entity references.
//!
//! # Quick start
//!
//! Implement [`Visitor`] to receive events, then feed input to a [`Reader`]:
//!
//! ```
//! use xml_syntax_reader::{Reader, Visitor, QName};
//!
//! struct Print;
//! impl Visitor for Print {
//!     type Error = std::convert::Infallible;
//!     fn start_tag_open(&mut self, name: QName<'_>) -> Result<(), Self::Error> {
//!         println!("element: {}", String::from_utf8_lossy(&name));
//!         Ok(())
//!     }
//! }
//!
//! let mut reader = Reader::new();
//! reader.parse_slice(b"<hello/>", &mut Print).unwrap();
//! ```
//!
//! For streaming use, call [`Reader::parse`] in a loop - it returns the
//! number of bytes consumed so the caller can shift the buffer and append
//! more data. [`parse_read`] wraps this loop for [`std::io::Read`] sources.
//!
//! # Encoding
//!
//! The parser operates on bytes and assumes UTF-8 input. Use
//! [`probe_encoding`] to detect the transport encoding (BOM / XML
//! declaration) and transcode if necessary before parsing.
//!
//! ## Input Limits
//!
//! The parser enforces hardcoded limits to prevent resource exhaustion:
//!
//! - **Names** (element, attribute, PI target, DOCTYPE, entity references):
//!   maximum **1,000 bytes**. Exceeding this produces [`ErrorKind::NameTooLong`].
//!
//! - **Character references**: maximum **7 bytes** for the value between
//!   `&#` and `;` (the longest valid reference is `&#x10FFFF;` or
//!   `&#1114111;`). Exceeding this produces [`ErrorKind::CharRefTooLong`].
//!
//! - **Text content, attribute values, and content bodies** (comments, CDATA
//!   sections, processing instructions, and DOCTYPE declarations) are all
//!   **streamed in chunks** at buffer boundaries. The visitor receives zero or
//!   more content calls with contiguous spans - zero for empty constructs
//!   (e.g. `<!---->`, `<?target?>`), and more than one when the body spans
//!   buffer boundaries. Text content (`characters`) is additionally
//!   interleaved with `entity_ref` / `char_ref` callbacks at reference
//!   boundaries. Attribute values are chunked at both buffer boundaries and
//!   entity/character reference boundaries, which produce separate
//!   `attribute_entity_ref` and `attribute_char_ref` callbacks. There is no
//!   size limit on any of these. See the [`Visitor`] trait documentation for
//!   the full callback sequences.

#[cfg(feature = "std")]
extern crate std;

#[forbid(unsafe_code)]
mod types;
#[forbid(unsafe_code)]
mod visitor;
#[forbid(unsafe_code)]
mod classify;
#[forbid(unsafe_code)]
mod state;
#[forbid(unsafe_code)]
mod reader;
#[forbid(unsafe_code)]
mod encoding;

mod bitstream;

pub use types::{DeclaredEncoding, Encoding, EntityKind, Error, ErrorKind, ParseError, QName, Span};
pub use visitor::Visitor;
pub use reader::Reader;
#[cfg(feature = "std")]
pub use reader::{parse_read, parse_read_with_capacity, ReadError};
pub use encoding::{probe_encoding, ProbeResult};
