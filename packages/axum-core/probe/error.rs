use std::path::PathBuf;

use thiserror::Error;

/// All typed failure modes for the probe pipeline.
///
/// Used throughout media/probe/. Upstream, EngineError gains a
/// `Probe(#[from] ProbeError)` variant so probe errors propagate cleanly
/// through the rest of the engine without ever becoming raw Strings.
#[derive(Debug, Error)]
pub enum ProbeError {
    /// ffprobe process failed to spawn or returned a non-zero exit code.
    /// The inner String is the captured stderr.
    #[error("ffprobe execution failed: {0}")]
    Execution(String),

    /// ffprobe stdout could not be parsed as valid JSON.
    #[error("ffprobe JSON parse error: {0}")]
    JsonParse(#[from] serde_json::Error),

    /// A required field was absent from the ffprobe JSON output.
    /// The &'static str names the missing field (e.g. "streams", "duration").
    #[error("missing required field in ffprobe output: {0}")]
    MissingData(&'static str),

    /// The file exists but contains no decodable video stream.
    #[error("no video stream found in file: {}", .0.display())]
    NoVideoStream(PathBuf),

    /// The file exists but ffprobe reports zero or negative duration.
    #[error("invalid duration ({duration}) for file: {}", .path.display())]
    InvalidDuration { path: PathBuf, duration: f64 },

    /// The file exists but has a zero or nonsensical resolution.
    #[error("invalid resolution ({width}x{height}) for file: {}", .path.display())]
    InvalidResolution {
        path: PathBuf,
        width: u32,
        height: u32,
    },

    /// The frame rate fraction has a zero denominator or zero numerator.
    #[error("invalid frame rate '{fps_str}' for file: {}", .path.display())]
    InvalidFrameRate { path: PathBuf, fps_str: String },

    /// The video codec or pixel format is unsupported by VidEngine's filter graph.
    #[error("unsupported codec or pixel format '{value}' in file: {}", .path.display())]
    UnsupportedFormat { path: PathBuf, value: String },

    /// The file path does not exist or is not readable.
    #[error("file not found or unreadable: {}", .0.display())]
    InvalidFile(PathBuf),

    /// An I/O error occurred while reading the file or spawning the process.
    #[error("I/O error during probe: {0}")]
    Io(#[from] std::io::Error),
}

impl ProbeError {
    /// Returns the file path associated with this error, if any.
    pub fn path(&self) -> Option<&PathBuf> {
        match self {
            Self::NoVideoStream(p)
            | Self::InvalidFile(p) => Some(p),
            Self::InvalidDuration { path, .. }
            | Self::InvalidResolution { path, .. }
            | Self::InvalidFrameRate { path, .. }
            | Self::UnsupportedFormat { path, .. } => Some(path),
            _ => None,
        }
    }

    /// Returns true if this error is likely transient (worth retrying).
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Execution(_) | Self::Io(_))
    }
}