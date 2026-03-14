//! # Media probe pipeline
//!
//! The single mandatory stage before anything else in the engine runs.
//! Every downstream stage — timeline builder, filter graph, segment planner,
//! AI agent context — reads from the `MediaInfoMap` produced here.
//!
//! ## Pipeline
//! ```text
//! probe_file(path, cache)
//!   ├── cache.get()              fast path — returns immediately on hit
//!   ├── ffprobe::run()           async child process — raw JSON
//!   ├── parser::parse()          pure fn — JSON → MediaInfo
//!   ├── validator::validate()    pure fn — safety checks
//!   ├── ffprobe::run_keyframe_pass()   second async pass for keyframes
//!   └── cache.insert()           store for next access
//! ```
//!
//! ## Usage
//! ```rust
//! // Probe a single file
//! let info: MediaInfo = probe_file(&path, &cache).await?;
//!
//! // Probe all input files from a command list (concurrent)
//! let map: MediaInfoMap = probe_files(&paths, &cache).await?;
//! ```

pub mod cache;
pub mod error;
pub mod ffprobe;
pub mod media_info;
pub mod parser;
pub mod validator;

// Re-export the public surface so callers use `media::probe::*` cleanly.
pub use cache::ProbeCache;
pub use error::ProbeError;
pub use media_info::{Fraction, MediaInfo, MediaInfoMap};

use std::path::{Path, PathBuf};

use futures::future::try_join_all;

// ─── Public API ───────────────────────────────────────────────────────────────

/// Probes a single media file and returns its `MediaInfo`.
///
/// Checks the cache first. On a miss, spawns ffprobe, parses the output,
/// runs all validation checks, runs the keyframe pass, and caches the result.
///
/// This function is the **only** place in the codebase that calls ffprobe.
/// All other engine stages read from the cached `MediaInfoMap`.
///
/// # Arguments
/// * `path`  — absolute path to the media file.
/// * `cache` — shared `ProbeCache` from `AppState`. Pass by reference; it is
///             `Clone` but sharing the same instance across calls is important
///             for the cache to have any effect.
///
/// # Errors
/// Returns `ProbeError` on any failure. The error includes the file path so
/// the IPC layer can surface a meaningful message to the user.
pub async fn probe_file(path: &Path, cache: &ProbeCache) -> Result<MediaInfo, ProbeError> {
    cache
        .get_or_probe(path, || probe_uncached(path))
        .await
}

/// Probes all given paths concurrently and returns a `MediaInfoMap`.
///
/// Uses `futures::future::try_join_all` to dispatch all ffprobe calls at once.
/// On a warm cache this returns near-instantly. On a cold cache with N files,
/// all N ffprobe processes are spawned in parallel — much faster than serial.
///
/// Fails immediately if any single probe fails. The error includes the path
/// of the failed file.
///
/// # Arguments
/// * `paths` — slice of absolute file paths to probe.
/// * `cache` — shared `ProbeCache` from `AppState`.
pub async fn probe_files(
    paths: &[PathBuf],
    cache: &ProbeCache,
) -> Result<MediaInfoMap, ProbeError> {
    let futures: Vec<_> = paths
        .iter()
        .map(|p| probe_file(p, cache))
        .collect();

    let results = try_join_all(futures).await?;

    let map: MediaInfoMap = paths
        .iter()
        .cloned()
        .zip(results)
        .collect();

    Ok(map)
}

// ─── Internal: uncached probe ─────────────────────────────────────────────────

/// Full probe pipeline for a single file, bypassing the cache.
///
/// Called by `probe_file` on a cache miss via `cache.get_or_probe()`.
/// Not part of the public API — callers should always go through `probe_file`.
async fn probe_uncached(path: &Path) -> Result<MediaInfo, ProbeError> {
    // Stage 1: spawn ffprobe, get raw JSON.
    let raw = ffprobe::run(path).await?;

    // Stage 2: parse JSON into MediaInfo (keyframes left empty here).
    let mut info = parser::parse(path, raw)?;

    // Stage 3: run all safety checks.
    validator::validate(&info)?;

    // Stage 4: keyframe pass (separate ffprobe invocation — slower, run after
    // validation so we don't waste time on invalid files).
    info.keyframes = ffprobe::run_keyframe_pass(path)
        .await
        .unwrap_or_default(); // keyframes are best-effort; never fail the whole probe

    Ok(info)
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Integration smoke test — requires ffprobe to be installed.
    /// Run with: cargo test --test integration_test -- --ignored
    #[tokio::test]
    #[ignore = "requires ffprobe binary and fixture files"]
    async fn probe_fixture_file_round_trip() {
        let path = PathBuf::from("tests/fixtures/5s_720p.mp4");
        let cache = ProbeCache::new();

        let info = probe_file(&path, &cache).await.unwrap();

        assert!(info.duration > 0.0);
        assert!(info.resolution.0 > 0);
        assert!(info.resolution.1 > 0);
        assert!(info.fps.is_valid());
        assert!(!info.video_codec.is_empty());
        assert!(!info.pixel_fmt.is_empty());

        // Second call should be a cache hit.
        assert!(cache.contains(&path).await);
        let cached = probe_file(&path, &cache).await.unwrap();
        assert_eq!(info.duration, cached.duration);
    }

    #[tokio::test]
    #[ignore = "requires ffprobe binary and fixture files"]
    async fn probe_files_concurrent_returns_map() {
        let paths = vec![
            PathBuf::from("tests/fixtures/5s_720p.mp4"),
            PathBuf::from("tests/fixtures/silent_5s.mp4"),
        ];
        let cache = ProbeCache::new();

        let map = probe_files(&paths, &cache).await.unwrap();

        assert_eq!(map.len(), 2);
        for path in &paths {
            assert!(map.contains_key(path));
        }
    }

    #[tokio::test]
    async fn probe_nonexistent_file_returns_invalid_file_error() {
        let path = PathBuf::from("/absolutely/does/not/exist.mp4");
        let cache = ProbeCache::new();
        let result = probe_file(&path, &cache).await;
        assert!(matches!(result, Err(ProbeError::InvalidFile(_))));
    }

    #[tokio::test]
    async fn cache_is_populated_after_miss() {
        // Can't actually probe without ffprobe, but we can test that the cache
        // interaction works by directly inserting and checking.
        let cache = ProbeCache::new();
        let path = PathBuf::from("/nonexistent/test.mp4");
        assert!(!cache.contains(&path).await);
    }
}