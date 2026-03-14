//! # Media module
//!
//! The `media` module is the public face of all media-related functionality
//! in the VidEngine engine crate. Currently it contains only the probe
//! sub-module, but is structured to accommodate future additions:
//!
//! ```text
//! media/
//! ├── mod.rs          ← this file
//! ├── probe/          ← ffprobe pipeline (current)
//! │   ├── mod.rs
//! │   ├── ffprobe.rs
//! │   ├── parser.rs
//! │   ├── media_info.rs
//! │   ├── error.rs
//! │   ├── validator.rs
//! │   └── cache.rs
//! ├── thumbnail.rs    ← future: single-frame PNG extraction for preview
//! └── transcode.rs    ← future: format normalisation before editing
//! ```
//!
//! ## Importing from other crates
//!
//! The engine re-exports the most commonly used types at the crate root,
//! so downstream crates (agent, ipc) use:
//!
//! ```rust
//! use videngine_engine::media::{MediaInfo, MediaInfoMap};
//! use videngine_engine::media::probe::{probe_file, probe_files, ProbeCache};
//! ```

pub mod probe;

// Flat re-exports — the types every downstream stage needs.
pub use probe::media_info::{Fraction, MediaInfo, MediaInfoMap};
pub use probe::{probe_file, probe_files, ProbeCache, ProbeError};