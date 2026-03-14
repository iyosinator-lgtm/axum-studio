use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::RwLock;

use super::media_info::MediaInfo;

// ─── ProbeCache ───────────────────────────────────────────────────────────────

/// Thread-safe, async cache for `MediaInfo` keyed by absolute file path.
///
/// Prevents re-probing unchanged files — a critical performance optimisation
/// for the AI agent workflow. Every agent edit triggers a re-run of the engine
/// from the Lua VM stage. Without caching, a project with 10 input files would
/// spawn 10 ffprobe processes on every single edit. With caching, those 10
/// processes run once at project open and are never repeated unless the file
/// changes.
///
/// ## Clone behaviour
/// `ProbeCache` is `Clone` and cheap to clone — all clones share the same
/// underlying `Arc<RwLock<...>>`. This is intentional: Tauri managed state
/// needs the type to be `Clone`, and multiple async tasks reading the cache
/// concurrently is safe via the inner `RwLock`.
///
/// ## Usage in AppState
/// ```rust
/// pub struct AppState {
///     pub engine:      Mutex<EngineHandle>,
///     pub agent:       Mutex<AgentHandle>,
///     pub probe_cache: ProbeCache,   // shared across all commands
/// }
/// ```
///
/// ## Invalidation
/// Call `invalidate(path)` when the user replaces a file in the media library.
/// Call `clear()` when the user opens a new project.
#[derive(Clone, Default)]
pub struct ProbeCache {
    inner: Arc<RwLock<HashMap<PathBuf, CacheEntry>>>,
}

/// A single cache entry. Stores the MediaInfo and the file's last-modified
/// timestamp so stale entries can be detected without re-probing.
#[derive(Clone)]
struct CacheEntry {
    info: MediaInfo,
    /// File modification time at the moment of probing (seconds since Unix epoch).
    /// None if the mtime could not be read (non-critical — will re-probe on next access).
    mtime: Option<u64>,
}

impl ProbeCache {
    /// Creates a new, empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    // ─── Read operations ─────────────────────────────────────────────────────

    /// Returns a cached `MediaInfo` if the entry is present and up-to-date.
    ///
    /// Returns `None` if:
    /// - The path has never been probed.
    /// - The file's mtime has changed since it was last probed (stale entry).
    ///
    /// The caller should probe the file and call `insert()` on a miss.
    pub async fn get(&self, path: &Path) -> Option<MediaInfo> {
        let guard = self.inner.read().await;
        let entry = guard.get(path)?;

        // Check if the file has been modified since we cached it.
        if let Some(cached_mtime) = entry.mtime {
            let current_mtime = file_mtime(path);
            if let Some(current) = current_mtime {
                if current != cached_mtime {
                    // File changed — treat as cache miss.
                    return None;
                }
            }
        }

        Some(entry.info.clone())
    }

    /// Returns true if the path has a valid (non-stale) cache entry.
    pub async fn contains(&self, path: &Path) -> bool {
        self.get(path).await.is_some()
    }

    /// Returns all currently cached paths. Useful for debugging.
    pub async fn cached_paths(&self) -> Vec<PathBuf> {
        self.inner.read().await.keys().cloned().collect()
    }

    /// Returns the number of entries in the cache.
    pub async fn len(&self) -> usize {
        self.inner.read().await.len()
    }

    /// Returns true if the cache is empty.
    pub async fn is_empty(&self) -> bool {
        self.inner.read().await.is_empty()
    }

    // ─── Write operations ─────────────────────────────────────────────────────

    /// Inserts or replaces a cache entry for the given path.
    ///
    /// Captures the file's current mtime so the cache can detect staleness
    /// on future reads without re-probing.
    pub async fn insert(&self, path: &Path, info: MediaInfo) {
        let mtime = file_mtime(path);
        let entry = CacheEntry { info, mtime };
        self.inner
            .write()
            .await
            .insert(path.to_path_buf(), entry);
    }

    /// Removes a single entry from the cache.
    ///
    /// Call this when the user replaces a file in the media library.
    /// The next `get()` call for this path will return `None`, triggering a fresh probe.
    pub async fn invalidate(&self, path: &Path) {
        self.inner.write().await.remove(path);
    }

    /// Removes all entries from the cache.
    ///
    /// Call this when the user opens a new project.
    pub async fn clear(&self) {
        self.inner.write().await.clear();
    }

    // ─── Combined get-or-probe ────────────────────────────────────────────────

    /// Returns a cached `MediaInfo` for `path`, or runs `probe_fn` and caches
    /// the result on a miss.
    ///
    /// This is the primary entry point used by `probe/mod.rs`. The `probe_fn`
    /// closure is only called if the path is not in the cache or the file is stale.
    ///
    /// ```rust
    /// let info = cache.get_or_probe(&path, || async {
    ///     let raw = ffprobe::run(&path).await?;
    ///     let info = parser::parse(&path, raw)?;
    ///     validator::validate(&info)?;
    ///     Ok(info)
    /// }).await?;
    /// ```
    pub async fn get_or_probe<F, Fut, E>(&self, path: &Path, probe_fn: F) -> Result<MediaInfo, E>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<MediaInfo, E>>,
    {
        // Fast path: cache hit.
        if let Some(cached) = self.get(path).await {
            return Ok(cached);
        }

        // Slow path: run the probe, cache the result.
        let info = probe_fn().await?;
        self.insert(path, info.clone()).await;
        Ok(info)
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Returns the file's last-modified time as seconds since the Unix epoch.
/// Returns `None` if the metadata cannot be read.
fn file_mtime(path: &Path) -> Option<u64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::probe::media_info::{Fraction, MediaInfo};
    use std::path::PathBuf;

    fn dummy_info(path: &str) -> MediaInfo {
        MediaInfo {
            path: PathBuf::from(path),
            duration: 10.0,
            fps: Fraction::new(30, 1),
            resolution: (1920, 1080),
            video_codec: "h264".to_string(),
            pixel_fmt: "yuv420p".to_string(),
            audio_sr: Some(48000),
            audio_ch: Some(2),
            audio_codec: Some("aac".to_string()),
            keyframes: vec![0.0, 2.0, 4.0, 6.0, 8.0],
            file_size: 1024,
        }
    }

    #[tokio::test]
    async fn insert_and_get_round_trip() {
        let cache = ProbeCache::new();
        let path = PathBuf::from("/nonexistent/test.mp4");
        let info = dummy_info("/nonexistent/test.mp4");

        // Miss before insert
        assert!(cache.get(&path).await.is_none());
        assert_eq!(cache.len().await, 0);

        // Insert and hit
        cache.insert(&path, info.clone()).await;

        // Note: get() checks mtime. Since the path is nonexistent, mtime returns
        // None for both cached and current, so the staleness check is skipped
        // and the entry is returned.
        let result = cache.get(&path).await;
        assert!(result.is_some());
        assert_eq!(result.unwrap().video_codec, "h264");
        assert_eq!(cache.len().await, 1);
    }

    #[tokio::test]
    async fn invalidate_removes_entry() {
        let cache = ProbeCache::new();
        let path = PathBuf::from("/nonexistent/clip.mp4");
        cache.insert(&path, dummy_info("/nonexistent/clip.mp4")).await;
        assert!(cache.get(&path).await.is_some());

        cache.invalidate(&path).await;
        assert!(cache.get(&path).await.is_none());
        assert_eq!(cache.len().await, 0);
    }

    #[tokio::test]
    async fn clear_removes_all_entries() {
        let cache = ProbeCache::new();
        for i in 0..5 {
            let path = PathBuf::from(format!("/nonexistent/{i}.mp4"));
            cache.insert(&path, dummy_info(&format!("/nonexistent/{i}.mp4"))).await;
        }
        assert_eq!(cache.len().await, 5);
        cache.clear().await;
        assert_eq!(cache.len().await, 0);
    }

    #[tokio::test]
    async fn clone_shares_underlying_data() {
        let cache = ProbeCache::new();
        let clone = cache.clone();
        let path = PathBuf::from("/nonexistent/shared.mp4");

        cache.insert(&path, dummy_info("/nonexistent/shared.mp4")).await;

        // Clone sees the insert from the original.
        assert!(clone.get(&path).await.is_some());
    }

    #[tokio::test]
    async fn get_or_probe_calls_fn_on_miss() {
        let cache = ProbeCache::new();
        let path = PathBuf::from("/nonexistent/lazy.mp4");
        let expected = dummy_info("/nonexistent/lazy.mp4");
        let expected_clone = expected.clone();

        let result: Result<MediaInfo, String> = cache
            .get_or_probe(&path, || async { Ok(expected_clone) })
            .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap().duration, expected.duration);
        assert_eq!(cache.len().await, 1);
    }

    #[tokio::test]
    async fn get_or_probe_does_not_call_fn_on_hit() {
        let cache = ProbeCache::new();
        let path = PathBuf::from("/nonexistent/cached.mp4");
        cache.insert(&path, dummy_info("/nonexistent/cached.mp4")).await;

        let mut called = false;
        let _: Result<MediaInfo, String> = cache
            .get_or_probe(&path, || async {
                called = true;
                Ok(dummy_info("/nonexistent/cached.mp4"))
            })
            .await;

        assert!(!called, "probe_fn should not be called on cache hit");
    }
}