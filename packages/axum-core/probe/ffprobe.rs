use std::path::Path;

use serde_json::Value;
use tokio::process::Command;

use super::error::ProbeError;

// ─── Main stream probe ────────────────────────────────────────────────────────

/// Spawns `ffprobe` as an **async** child process and returns the raw JSON output.
///
/// Uses `tokio::process::Command` so this never blocks the Tauri runtime or the
/// UI thread during project load or AI agent edits.
///
/// The caller (probe/mod.rs) is responsible for passing this output to
/// `parser::parse()` and then `validator::validate()`.
///
/// # Arguments
/// * `path` — absolute path to the media file to probe.
///
/// # Errors
/// Returns `ProbeError::InvalidFile` if the path does not exist.
/// Returns `ProbeError::Execution` if ffprobe exits non-zero.
/// Returns `ProbeError::JsonParse` if the stdout is not valid JSON.
pub async fn run(path: &Path) -> Result<Value, ProbeError> {
    // Verify the file exists before spawning a child process.
    if !path.exists() {
        return Err(ProbeError::InvalidFile(path.to_path_buf()));
    }

    let output = Command::new("ffprobe")
        .args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_streams",
            "-show_format",
        ])
        .arg(path)
        .output()
        .await
        .map_err(|e| ProbeError::Execution(format!("failed to spawn ffprobe: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        return Err(ProbeError::Execution(format!(
            "ffprobe exited with status {}: {stderr}",
            output.status
        )));
    }

    let json: Value = serde_json::from_slice(&output.stdout)?;
    Ok(json)
}

// ─── Keyframe probe ───────────────────────────────────────────────────────────

/// Runs a second ffprobe pass to extract keyframe (I-frame) timestamps.
///
/// This is separate from `run()` because it is significantly slower — it must
/// read the entire file to find all I-frame positions. The result is stored in
/// `MediaInfo.keyframes` and used by the segment planner to find clean GOP
/// boundaries for splits.
///
/// Only called once per file per project session (results are cached).
///
/// # Arguments
/// * `path` — absolute path to the media file.
///
/// # Returns
/// Sorted `Vec<f64>` of keyframe timestamps in seconds. May be empty for
/// formats where ffprobe cannot extract keyframe data (e.g. some MKV files).
pub async fn run_keyframe_pass(path: &Path) -> Result<Vec<f64>, ProbeError> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-select_streams",
            "v:0",
            "-show_entries",
            "packet=pts_time,flags",
            "-skip_frame",
            "nokey",
        ])
        .arg(path)
        .output()
        .await
        .map_err(|e| ProbeError::Execution(format!("keyframe pass failed to spawn: {e}")))?;

    if !output.status.success() {
        // Keyframe extraction is best-effort — return empty rather than hard fail.
        // The segment planner handles an empty keyframes list gracefully.
        return Ok(vec![]);
    }

    let json: Value = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);

    let mut keyframes: Vec<f64> = json["packets"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|pkt| {
            // Only keep packets flagged as keyframes.
            let flags = pkt["flags"].as_str().unwrap_or("");
            if !flags.contains('K') {
                return None;
            }
            pkt["pts_time"]
                .as_str()
                .and_then(|s| s.parse::<f64>().ok())
        })
        .collect();

    keyframes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    keyframes.dedup_by(|a, b| (*a - *b).abs() < 0.001);

    Ok(keyframes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn run_returns_invalid_file_for_nonexistent_path() {
        let path = PathBuf::from("/tmp/this_file_does_not_exist_videngine.mp4");
        let result = run(&path).await;
        assert!(matches!(result, Err(ProbeError::InvalidFile(_))));
    }
}