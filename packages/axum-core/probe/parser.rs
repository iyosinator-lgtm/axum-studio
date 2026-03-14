use std::path::{Path, PathBuf};

use serde_json::Value;

use super::error::ProbeError;
use super::media_info::{Fraction, MediaInfo};

// ─── Public entry point ───────────────────────────────────────────────────────

/// Converts raw ffprobe JSON output into a clean `MediaInfo` struct.
///
/// This is a pure function — it does not spawn any processes. The keyframes
/// field is left empty here; it is filled in by a separate call to
/// `ffprobe::run_keyframe_pass()` in the probe pipeline.
///
/// # Arguments
/// * `path`  — the file that was probed (stored in MediaInfo for reference).
/// * `raw`   — the JSON value returned by `ffprobe::run()`.
///
/// # Errors
/// Returns `ProbeError::MissingData` if required fields are absent.
/// Returns `ProbeError::NoVideoStream` if no video stream is found.
pub fn parse(path: &Path, raw: Value) -> Result<MediaInfo, ProbeError> {
    let streams = raw["streams"]
        .as_array()
        .ok_or(ProbeError::MissingData("streams"))?;

    let format = &raw["format"];

    let video = extract_video_stream(path, streams)?;
    let audio = extract_audio_stream(streams);

    let duration = parse_duration(path, format, streams)?;
    let file_size = parse_file_size(format);

    Ok(MediaInfo {
        path: path.to_path_buf(),
        duration,
        fps: video.fps,
        resolution: (video.width, video.height),
        video_codec: video.codec_name,
        pixel_fmt: video.pix_fmt,
        audio_sr: audio.as_ref().map(|a| a.sample_rate),
        audio_ch: audio.as_ref().map(|a| a.channels),
        audio_codec: audio.map(|a| a.codec_name),
        keyframes: vec![], // filled by keyframe pass in probe/mod.rs
        file_size,
    })
}

// ─── Internal structs ─────────────────────────────────────────────────────────

struct VideoStream {
    codec_name: String,
    width: u32,
    height: u32,
    fps: Fraction,
    pix_fmt: String,
}

struct AudioStream {
    codec_name: String,
    sample_rate: u32,
    channels: u8,
}

// ─── Stream extraction ────────────────────────────────────────────────────────

fn extract_video_stream(path: &Path, streams: &[Value]) -> Result<VideoStream, ProbeError> {
    let stream = streams
        .iter()
        .find(|s| s["codec_type"].as_str() == Some("video"))
        .ok_or_else(|| ProbeError::NoVideoStream(path.to_path_buf()))?;

    let codec_name = stream["codec_name"]
        .as_str()
        .ok_or(ProbeError::MissingData("video codec_name"))?
        .to_string();

    let width = stream["width"]
        .as_u64()
        .ok_or(ProbeError::MissingData("width"))? as u32;

    let height = stream["height"]
        .as_u64()
        .ok_or(ProbeError::MissingData("height"))? as u32;

    // ffprobe exposes both avg_frame_rate and r_frame_rate.
    // avg_frame_rate is more reliable for VFR content.
    let fps_str = stream["avg_frame_rate"]
        .as_str()
        .unwrap_or("0/1")
        .to_string();

    let fps = parse_fps(path, &fps_str)?;

    let pix_fmt = stream["pix_fmt"]
        .as_str()
        .unwrap_or("yuv420p")
        .to_string();

    Ok(VideoStream {
        codec_name,
        width,
        height,
        fps,
        pix_fmt,
    })
}

fn extract_audio_stream(streams: &[Value]) -> Option<AudioStream> {
    let stream = streams
        .iter()
        .find(|s| s["codec_type"].as_str() == Some("audio"))?;

    let codec_name = stream["codec_name"].as_str()?.to_string();

    let sample_rate: u32 = stream["sample_rate"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .unwrap_or(44100);

    let channels: u8 = stream["channels"]
        .as_u64()
        .unwrap_or(2)
        .try_into()
        .unwrap_or(2);

    Some(AudioStream {
        codec_name,
        sample_rate,
        channels,
    })
}

// ─── Field parsers ────────────────────────────────────────────────────────────

/// Parses a frame rate string like "30000/1001" or "30/1" into a `Fraction`.
///
/// ffprobe always returns frame rates as "num/den" strings. Handles the edge
/// case where den is 0 by returning `ProbeError::InvalidFrameRate`.
fn parse_fps(path: &Path, fps_str: &str) -> Result<Fraction, ProbeError> {
    let parts: Vec<&str> = fps_str.split('/').collect();

    match parts.as_slice() {
        [num_str, den_str] => {
            let num: u32 = num_str.parse().unwrap_or(0);
            let den: u32 = den_str.parse().unwrap_or(0);

            if num == 0 || den == 0 {
                return Err(ProbeError::InvalidFrameRate {
                    path: path.to_path_buf(),
                    fps_str: fps_str.to_string(),
                });
            }

            Ok(Fraction::new(num, den))
        }
        // Single number fallback (e.g. "30")
        [num_str] => {
            let num: u32 = num_str.parse().unwrap_or(0);
            if num == 0 {
                return Err(ProbeError::InvalidFrameRate {
                    path: path.to_path_buf(),
                    fps_str: fps_str.to_string(),
                });
            }
            Ok(Fraction::new(num, 1))
        }
        _ => Err(ProbeError::InvalidFrameRate {
            path: path.to_path_buf(),
            fps_str: fps_str.to_string(),
        }),
    }
}

/// Extracts duration in seconds from the ffprobe format block.
///
/// Falls back to summing stream durations if format.duration is missing,
/// which can happen for some container formats (e.g. certain MKV files).
fn parse_duration(path: &Path, format: &Value, streams: &[Value]) -> Result<f64, ProbeError> {
    // Primary: format-level duration (most accurate)
    if let Some(d) = format["duration"]
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
    {
        if d > 0.0 {
            return Ok(d);
        }
    }

    // Fallback: max stream duration
    let stream_duration = streams
        .iter()
        .filter_map(|s| {
            s["duration"]
                .as_str()
                .and_then(|d| d.parse::<f64>().ok())
        })
        .fold(0.0_f64, f64::max);

    if stream_duration > 0.0 {
        return Ok(stream_duration);
    }

    Err(ProbeError::InvalidDuration {
        path: path.to_path_buf(),
        duration: 0.0,
    })
}

/// Extracts file size in bytes from the ffprobe format block. Returns 0 on
/// failure (non-critical — file size is informational only).
fn parse_file_size(format: &Value) -> u64 {
    format["size"]
        .as_str()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0)
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    fn sample_json() -> Value {
        json!({
            "streams": [
                {
                    "codec_type": "video",
                    "codec_name": "h264",
                    "width": 1920,
                    "height": 1080,
                    "avg_frame_rate": "30000/1001",
                    "pix_fmt": "yuv420p"
                },
                {
                    "codec_type": "audio",
                    "codec_name": "aac",
                    "sample_rate": "48000",
                    "channels": 2
                }
            ],
            "format": {
                "duration": "15.032000",
                "size": "25165824"
            }
        })
    }

    #[test]
    fn parse_standard_h264_mp4() {
        let path = PathBuf::from("/tmp/test.mp4");
        let info = parse(&path, sample_json()).unwrap();

        assert_eq!(info.video_codec, "h264");
        assert_eq!(info.resolution, (1920, 1080));
        assert_eq!(info.fps, Fraction::new(30000, 1001));
        assert!((info.fps.as_f64() - 29.97).abs() < 0.01);
        assert_eq!(info.pixel_fmt, "yuv420p");
        assert!((info.duration - 15.032).abs() < 0.001);
        assert_eq!(info.audio_sr, Some(48000));
        assert_eq!(info.audio_ch, Some(2));
        assert_eq!(info.audio_codec, Some("aac".to_string()));
        assert_eq!(info.file_size, 25165824);
        assert!(info.keyframes.is_empty()); // filled by keyframe pass
    }

    #[test]
    fn parse_video_only_file() {
        let path = PathBuf::from("/tmp/silent.mp4");
        let raw = json!({
            "streams": [{
                "codec_type": "video",
                "codec_name": "h264",
                "width": 1280,
                "height": 720,
                "avg_frame_rate": "60/1",
                "pix_fmt": "yuv420p"
            }],
            "format": { "duration": "5.0" }
        });
        let info = parse(&path, raw).unwrap();
        assert!(!info.has_audio());
        assert_eq!(info.fps, Fraction::new(60, 1));
    }

    #[test]
    fn parse_returns_no_video_stream_error() {
        let path = PathBuf::from("/tmp/audio_only.mp3");
        let raw = json!({
            "streams": [{
                "codec_type": "audio",
                "codec_name": "mp3",
                "sample_rate": "44100",
                "channels": 2
            }],
            "format": { "duration": "180.0" }
        });
        let result = parse(&path, raw);
        assert!(matches!(result, Err(ProbeError::NoVideoStream(_))));
    }

    #[test]
    fn parse_fps_fraction_variants() {
        let path = PathBuf::from("/tmp/x.mp4");
        assert_eq!(parse_fps(&path, "30000/1001").unwrap(), Fraction::new(30000, 1001));
        assert_eq!(parse_fps(&path, "30/1").unwrap(), Fraction::new(30, 1));
        assert_eq!(parse_fps(&path, "25/1").unwrap(), Fraction::new(25, 1));
        assert!(parse_fps(&path, "0/1").is_err());
        assert!(parse_fps(&path, "30/0").is_err());
        assert!(parse_fps(&path, "garbage").is_err());
    }

    #[test]
    fn parse_duration_fallback_to_stream() {
        let path = PathBuf::from("/tmp/x.mkv");
        let raw = json!({
            "streams": [
                {
                    "codec_type": "video",
                    "codec_name": "hevc",
                    "width": 3840,
                    "height": 2160,
                    "avg_frame_rate": "24/1",
                    "pix_fmt": "yuv420p",
                    "duration": "120.5"
                }
            ],
            "format": {}  // no format-level duration
        });
        let info = parse(&path, raw).unwrap();
        assert!((info.duration - 120.5).abs() < 0.001);
    }
}