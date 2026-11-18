//! Standalone HyperDoc reader with memory-mapped, safe positional-file, and
//! in-memory byte sources. The format implementation is local to this crate;
//! it does not depend on `hdoc-encoder`.

mod cache;
mod error;
mod reader;
mod source;
mod vector_path;

pub mod format;
pub mod opcodes;
pub mod search;
pub mod varint;

pub use error::{DecoderError, HdocError};
pub use opcodes::{GlyphOffset, PageCommand, PageDisplayList, SvgGlyphStyle, SvgGlyphUse};
pub use reader::{HdocDocument, RankedSearchResult, SearchResult};
pub use search::SearchBlock;
pub use source::{ByteSource, FileSource, MemorySource, MmapSource};
