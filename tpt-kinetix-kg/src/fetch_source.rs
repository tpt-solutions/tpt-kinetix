//! Pinned-commit fetch of individual FFmpeg source files for table verification.
//!
//! FFmpeg is LGPL/GPL; this workspace is Apache/MIT. To avoid distributing
//! GPL-licensed source inside a permissively-licensed repo, fetched files are
//! written to a gitignored cache directory (never committed) and re-fetched on
//! demand rather than vendored. See [`crate::table_extract`] for what they're
//! used for.

use std::path::{Path, PathBuf};

/// Fetch `libavcodec/<file>` (relative path under FFmpeg's tree) at `commit`
/// from GitHub, writing it under `cache_dir/<commit>/<file>` and returning that
/// path. If the file already exists in the cache, it is reused without a
/// network round-trip (the commit hash pins the content, so this is safe).
pub fn fetch_pinned_file(commit: &str, file: &str, cache_dir: &Path) -> anyhow::Result<PathBuf> {
    let out_path = cache_dir.join(commit).join(file);
    if out_path.is_file() {
        return Ok(out_path);
    }

    let url = format!("https://raw.githubusercontent.com/FFmpeg/FFmpeg/{commit}/{file}");
    let body = ureq::get(&url)
        .call()
        .map_err(|e| anyhow::anyhow!("fetching {url}: {e}"))?
        .into_body()
        .read_to_string()
        .map_err(|e| anyhow::anyhow!("reading response body from {url}: {e}"))?;

    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&out_path, &body)?;
    Ok(out_path)
}
