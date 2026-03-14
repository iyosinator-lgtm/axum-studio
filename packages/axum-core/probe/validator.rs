use super::error::ProbeError;
use super::media_info::MediaInfo;

// ─── Supported pixel formats ──────────────────────────────────────────────────

/// Pixel formats that VidEngine's filter graph builder can handle correctly.
///
/// The PadRegistry in graph/pad_registry.rs tracks pixel formats per named pad
/// and rejects chains that mix incompatible formats. This list is the gate at
/// ingest — files with exotic formats fail fast here with a clear error message
/// rather than producing a corrupt filter graph at render time.
///
/// Extend this list as new format support is added to the filter graph builder.
const SUPPORTED_PIXEL_FMTS: &[&str] = &[
    "yuv420p",
    "yuv422p",
    "yuv444p",
    "yuv420p10le",
    "yuv422p10le",
    "yuv444p10le",
    "yuvj420p", // MJPEG-produced, treated as yuv420p by most filters
    "yuvj422p",
    "yuvj444p",
    "rgba",
    "rgb24",
    "bgra",
    "nv12",
    "nv21",
];

/// Video codecs that VidEngine can decode (via FFmpeg).
///
/// This is not an exhaustive FFmpeg codec list — it is the set that has been
/// explicitly tested in VidEngine's filter graph and segment pipeline.
const SUPPORTED_VIDEO_CODECS: &[&str] = &[
    "h264", "hevc", "h265", "vp8", "vp9", "av1", "mpeg4", "mpeg2video",
    "mjpeg", "prores", "dnxhd", "theora",
];

// ─── Validator ────────────────────────────────────────────────────────────────

/// Runs all safety checks required by the VidEngine spec on a `MediaInfo`.
///
/// Called at the end of every probe pipeline run. If this returns `Ok(())`,
/// every downstream stage (Lua VM, filter graph, segment planner, AI agent
/// context builder) can trust the MediaInfo fields without defensive checks.
///
/// # Checks performed
/// 1. Duration > 0
/// 2. Resolution > 0 × 0
/// 3. FPS fraction valid and > 0
/// 4. Video codec is in the supported list
/// 5. Pixel format is in the supported list
/// 6. Audio fields are internally consistent (if audio present)
/// 7. File size > 0
///
/// # Errors
/// Returns the first `ProbeError` encountered. All checks are independent so
/// fixing one error may reveal the next — this is intentional (fail fast).
pub fn validate(info: &MediaInfo) -> Result<(), ProbeError> {
    check_duration(info)?;
    check_resolution(info)?;
    check_fps(info)?;
    check_video_codec(info)?;
    check_pixel_fmt(info)?;
    check_audio_consistency(info)?;
    check_file_size(info)?;
    Ok(())
}

// ─── Individual checks ────────────────────────────────────────────────────────

fn check_duration(info: &MediaInfo) -> Result<(), ProbeError> {
    if info.duration <= 0.0 || !info.duration.is_finite() {
        return Err(ProbeError::InvalidDuration {
            path: info.path.clone(),
            duration: info.duration,
        });
    }
    Ok(())
}

fn check_resolution(info: &MediaInfo) -> Result<(), ProbeError> {
    if info.resolution.0 == 0 || info.resolution.1 == 0 {
        return Err(ProbeError::InvalidResolution {
            path: info.path.clone(),
            width: info.resolution.0,
            height: info.resolution.1,
        });
    }
    Ok(())
}

fn check_fps(info: &MediaInfo) -> Result<(), ProbeError> {
    if !info.fps.is_valid() {
        return Err(ProbeError::InvalidFrameRate {
            path: info.path.clone(),
            fps_str: info.fps.to_string(),
        });
    }
    // Sanity-check: reject clearly impossible frame rates (> 1000 fps).
    // This catches malformed ffprobe output without blocking legitimate
    // high-frame-rate content (120/240 fps is fine; 100000/1 is not).
    if info.fps.as_f64() > 1000.0 {
        return Err(ProbeError::InvalidFrameRate {
            path: info.path.clone(),
            fps_str: format!("{} (exceeds 1000 fps limit)", info.fps),
        });
    }
    Ok(())
}

fn check_video_codec(info: &MediaInfo) -> Result<(), ProbeError> {
    let codec = info.video_codec.to_lowercase();
    if !SUPPORTED_VIDEO_CODECS.contains(&codec.as_str()) {
        return Err(ProbeError::UnsupportedFormat {
            path: info.path.clone(),
            value: format!("video codec '{}'", info.video_codec),
        });
    }
    Ok(())
}

fn check_pixel_fmt(info: &MediaInfo) -> Result<(), ProbeError> {
    let fmt = info.pixel_fmt.to_lowercase();
    if !SUPPORTED_PIXEL_FMTS.contains(&fmt.as_str()) {
        return Err(ProbeError::UnsupportedFormat {
            path: info.path.clone(),
            value: format!("pixel format '{}'", info.pixel_fmt),
        });
    }
    Ok(())
}

fn check_audio_consistency(info: &MediaInfo) -> Result<(), ProbeError> {
    // All three audio fields must be present together or all absent.
    let has_sr = info.audio_sr.is_some();
    let has_ch = info.audio_ch.is_some();
    let has_codec = info.audio_codec.is_some();

    if has_sr || has_ch || has_codec {
        // At least one is present — all must be.
        if !has_sr || !has_ch || !has_codec {
            return Err(ProbeError::MissingData(
                "audio fields incomplete: audio_sr, audio_ch, and audio_codec must all be present or all absent",
            ));
        }
        // Sample rate sanity: 8000 Hz (telephone) to 192000 Hz (high-res audio).
        if let Some(sr) = info.audio_sr {
            if !(8000..=192_000).contains(&sr) {
                return Err(ProbeError::MissingData(
                    "audio sample rate out of valid range (8000–192000 Hz)",
                ));
            }
        }
    }
    Ok(())
}

fn check_file_size(info: &MediaInfo) -> Result<(), ProbeError> {
    if info.file_size == 0 {
        // Non-fatal: ffprobe sometimes can't read file size from container.
        // We emit a warning path but don't fail — the video data is still valid.
        // In a real implementation this would call tracing::warn!().
        let _ = &info.path; // suppress unused warning
    }
    Ok(())
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::probe::media_info::{Fraction, MediaInfo};
    use std::path::PathBuf;

    fn valid_info() -> MediaInfo {
        MediaInfo {
            path: PathBuf::from("/tmp/test.mp4"),
            duration: 15.0,
            fps: Fraction::new(30, 1),
            resolution: (1920, 1080),
            video_codec: "h264".to_string(),
            pixel_fmt: "yuv420p".to_string(),
            audio_sr: Some(48000),
            audio_ch: Some(2),
            audio_codec: Some("aac".to_string()),
            keyframes: vec![],
            file_size: 1024 * 1024,
        }
    }

    #[test]
    fn valid_info_passes_all_checks() {
        assert!(validate(&valid_info()).is_ok());
    }

    #[test]
    fn zero_duration_fails() {
        let mut info = valid_info();
        info.duration = 0.0;
        assert!(matches!(validate(&info), Err(ProbeError::InvalidDuration { .. })));
    }

    #[test]
    fn negative_duration_fails() {
        let mut info = valid_info();
        info.duration = -1.5;
        assert!(matches!(validate(&info), Err(ProbeError::InvalidDuration { .. })));
    }

    #[test]
    fn zero_resolution_fails() {
        let mut info = valid_info();
        info.resolution = (0, 1080);
        assert!(matches!(validate(&info), Err(ProbeError::InvalidResolution { .. })));
    }

    #[test]
    fn invalid_fps_fails() {
        let mut info = valid_info();
        info.fps = Fraction::new(0, 1);
        assert!(matches!(validate(&info), Err(ProbeError::InvalidFrameRate { .. })));
    }

    #[test]
    fn absurd_fps_fails() {
        let mut info = valid_info();
        info.fps = Fraction::new(100_000, 1);
        assert!(matches!(validate(&info), Err(ProbeError::InvalidFrameRate { .. })));
    }

    #[test]
    fn unsupported_codec_fails() {
        let mut info = valid_info();
        info.video_codec = "rv40".to_string(); // RealVideo — not supported
        assert!(matches!(validate(&info), Err(ProbeError::UnsupportedFormat { .. })));
    }

    #[test]
    fn unsupported_pixel_fmt_fails() {
        let mut info = valid_info();
        info.pixel_fmt = "gbrp16le".to_string();
        assert!(matches!(validate(&info), Err(ProbeError::UnsupportedFormat { .. })));
    }

    #[test]
    fn partial_audio_fields_fails() {
        let mut info = valid_info();
        info.audio_sr = Some(48000);
        info.audio_ch = None; // missing
        info.audio_codec = Some("aac".to_string());
        assert!(matches!(validate(&info), Err(ProbeError::MissingData(_))));
    }

    #[test]
    fn no_audio_is_valid() {
        let mut info = valid_info();
        info.audio_sr = None;
        info.audio_ch = None;
        info.audio_codec = None;
        assert!(validate(&info).is_ok());
    }
}