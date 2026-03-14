use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

// ─── Fraction ────────────────────────────────────────────────────────────────

/// Exact rational representation of a frame rate (e.g. 30000/1001 for 29.97).
/// Stored as-is from ffprobe to avoid float precision loss.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fraction {
    pub num: u32,
    pub den: u32,
}

impl Fraction {
    pub fn new(num: u32, den: u32) -> Self {
        Self { num, den }
    }

    /// Returns the frame rate as a float. Panics if den == 0 (validator prevents this).
    pub fn as_f64(&self) -> f64 {
        self.num as f64 / self.den as f64
    }

    /// Returns true if this fraction represents a valid, non-zero frame rate.
    pub fn is_valid(&self) -> bool {
        self.den != 0 && self.num != 0
    }
}

impl fmt::Display for Fraction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.num, self.den)
    }
}

impl Default for Fraction {
    fn default() -> Self {
        Self { num: 30, den: 1 }
    }
}

// ─── MediaInfo ───────────────────────────────────────────────────────────────

/// Complete metadata for a single media file.
///
/// Produced by the probe pipeline and consumed by every downstream stage:
/// - Timeline builder (source_in/source_out validation)
/// - Filter graph builder (pixel format, fps, resolution)
/// - Segment planner (keyframe timestamps, duration)
/// - AI agent context (serialised as JSON in the LLM system prompt)
/// - Output validator (expected duration comparison)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaInfo {
    /// Absolute path to the source file.
    pub path: PathBuf,

    /// Duration in seconds (from ffprobe format.duration).
    pub duration: f64,

    /// Exact frame rate as a rational fraction (e.g. 30000/1001).
    pub fps: Fraction,

    /// Video resolution in pixels: (width, height).
    pub resolution: (u32, u32),

    /// FFmpeg codec name for the video stream (e.g. "h264", "hevc", "vp9").
    pub video_codec: String,

    /// FFmpeg pixel format string (e.g. "yuv420p", "yuv444p", "rgba").
    /// Used by PadRegistry to validate filter chain compatibility.
    pub pixel_fmt: String,

    /// Audio sample rate in Hz (e.g. 44100, 48000). None if no audio stream.
    pub audio_sr: Option<u32>,

    /// Number of audio channels (1 = mono, 2 = stereo). None if no audio stream.
    pub audio_ch: Option<u8>,

    /// Audio codec name (e.g. "aac", "mp3", "flac"). None if no audio stream.
    pub audio_codec: Option<String>,

    /// Keyframe timestamps in seconds, sorted ascending.
    /// Populated by a separate ffprobe pass. Used by segment planner to find
    /// clean split points that don't land mid-GOP.
    pub keyframes: Vec<f64>,

    /// File size in bytes.
    pub file_size: u64,
}

impl MediaInfo {
    /// Returns true if the file has at least one audio stream.
    pub fn has_audio(&self) -> bool {
        self.audio_sr.is_some()
    }

    /// Returns the frame rate as a float. Convenience wrapper over fps.as_f64().
    pub fn fps_f64(&self) -> f64 {
        self.fps.as_f64()
    }

    /// Returns the video width.
    pub fn width(&self) -> u32 {
        self.resolution.0
    }

    /// Returns the video height.
    pub fn height(&self) -> u32 {
        self.resolution.1
    }

    /// Returns true if the pixel format is YUV 4:2:0 (the most common H.264 format).
    pub fn is_yuv420p(&self) -> bool {
        self.pixel_fmt == "yuv420p"
    }

    /// Returns the nearest keyframe at or before the given timestamp.
    /// Returns 0.0 if keyframes is empty or ts < first keyframe.
    pub fn nearest_keyframe_before(&self, ts: f64) -> f64 {
        self.keyframes
            .iter()
            .rev()
            .find(|&&kf| kf <= ts)
            .copied()
            .unwrap_or(0.0)
    }
}

// ─── MediaInfoMap ────────────────────────────────────────────────────────────

/// Map from file path to its probed metadata.
///
/// Passed as a shared reference through all engine stages. Every stage that
/// needs metadata for a file (timeline builder, graph builder, planner) reads
/// from this map — they never call ffprobe directly.
pub type MediaInfoMap = HashMap<PathBuf, MediaInfo>;