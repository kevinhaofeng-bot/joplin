# Local xml-syntax-reader patch

This directory retains the `xml-syntax-reader` 0.2.0 crate source and README
from crates.io. Its Cargo metadata declares `MIT/Apache-2.0` licensing and
identifies https://github.com/dholroyd/xml-syntax-reader as upstream. The
published crate did not include separate license files.

Only `src/reader.rs::try_inline_with_peek` changes parser behavior: after an
inline tag it marks the following text start before scanning for the next tag.
Without this, short MIME/filename text between consecutive inline tags can be
silently omitted. The ENEX integration tests exercise resource data and
metadata across multiple refill offsets, including a 22 MiB resource.

The two `cfg(feature = "dtd")` annotations in `src/reader.rs` suppress existing
unused-code warnings when the default, non-DTD feature set is built.
