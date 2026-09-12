//! OpenIt document library.
//!
//! Everything a document needs that does not involve a window: what kind of
//! file it is, how to load it, how to save it without corrupting the original,
//! and later how drafts, resources, and schemas are handled. No GPUI here.

pub mod associations;
pub mod browse;
pub mod cache;
pub mod catalog;
pub mod document;
pub mod error;
pub mod handoff;
pub mod ipc;
pub mod kind;
pub mod pdf;
pub mod pdf_markdown;
pub mod pdf_text;
pub mod raster;
pub mod recovery;
pub mod resource;
pub mod save;
pub mod schema;
pub mod select;
pub mod session;
pub mod settings;
pub mod theme;
pub mod watch;

pub use cache::ResourceCache;
pub use error::Error;
