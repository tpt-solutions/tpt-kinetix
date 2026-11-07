//! Node.js bindings for the TPT Kinetix container tools.
//!
//! The native addon exposes `probeJson`, `faststart`, `remux` and `package`. The
//! long-running ones (`faststart`, `remux`, `package`) run on libuv's thread pool and
//! return promises, so they do not block the event loop. `index.js` wraps these into
//! the public API (`probe` parses the JSON, `package` returns a plain object).

use std::path::{Path, PathBuf};

use napi::bindgen_prelude::{AsyncTask, Env, Task};
use napi::{Error, Result, Status};
use napi_derive::napi;

fn to_napi(e: impl std::fmt::Display) -> Error {
    Error::new(Status::GenericFailure, e.to_string())
}

/// The library version, e.g. `0.1.2`.
#[napi]
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Lists the tracks of a WebM, Matroska, IVF or MP4 file as a JSON string.
#[napi]
pub fn probe_json(path: String) -> Result<String> {
    tpt_kinetix_ops::summary::summary_json(Path::new(&path)).map_err(to_napi)
}

/// Counts returned by `package`.
#[napi(object)]
pub struct PackageCounts {
    /// Number of packaged tracks.
    pub tracks: u32,
    /// Number of segments per track.
    pub segments: u32,
    /// Files written into the output directory.
    pub files: u32,
    /// Bytes of media written.
    pub bytes: f64,
}

pub struct FaststartTask {
    input: PathBuf,
    output: PathBuf,
}

impl Task for FaststartTask {
    type Output = ();
    type JsValue = ();

    fn compute(&mut self) -> Result<()> {
        tpt_kinetix_ops::faststart_file(&self.input, &self.output).map_err(to_napi)
    }

    fn resolve(&mut self, _: Env, _: ()) -> Result<()> {
        Ok(())
    }
}

/// Writes a copy of `input` with the MP4 index moved to the front.
#[napi(ts_return_type = "Promise<void>")]
pub fn faststart(input: String, output: String) -> AsyncTask<FaststartTask> {
    AsyncTask::new(FaststartTask {
        input: input.into(),
        output: output.into(),
    })
}

pub struct RemuxTask {
    input: String,
    output: PathBuf,
    faststart: bool,
    fragment_ms: Option<u32>,
}

impl Task for RemuxTask {
    type Output = ();
    type JsValue = ();

    fn compute(&mut self) -> Result<()> {
        tpt_kinetix_ops::remux::remux(&self.input, &self.output, self.faststart, self.fragment_ms)
            .map_err(to_napi)
    }

    fn resolve(&mut self, _: Env, _: ()) -> Result<()> {
        Ok(())
    }
}

/// Copies the streams of `input` into `output` without decoding. The container follows
/// the extension of `output`. `fragmentMs` greater than zero writes fragmented MP4.
#[napi(ts_return_type = "Promise<void>")]
pub fn remux(
    input: String,
    output: String,
    faststart: bool,
    fragment_ms: Option<u32>,
) -> AsyncTask<RemuxTask> {
    AsyncTask::new(RemuxTask {
        input,
        output: output.into(),
        faststart,
        fragment_ms: fragment_ms.filter(|&ms| ms > 0),
    })
}

pub struct PackageTask {
    input: String,
    out_dir: PathBuf,
    segment_seconds: f64,
    key_id: Option<String>,
    key: Option<String>,
    scheme: String,
}

impl Task for PackageTask {
    type Output = PackageCounts;
    type JsValue = PackageCounts;

    fn compute(&mut self) -> Result<PackageCounts> {
        let encryption = tpt_kinetix_ops::encryption_from_hex(
            self.key_id.as_deref(),
            self.key.as_deref(),
            &self.scheme,
        )
        .map_err(to_napi)?;
        let done = tpt_kinetix_ops::package::package(
            &self.input,
            &self.out_dir,
            self.segment_seconds,
            encryption,
        )
        .map_err(to_napi)?;
        Ok(PackageCounts {
            tracks: done.tracks as u32,
            segments: done.segments as u32,
            files: done.files as u32,
            bytes: done.bytes as f64,
        })
    }

    fn resolve(&mut self, _: Env, output: PackageCounts) -> Result<PackageCounts> {
        Ok(output)
    }
}

/// Writes an HLS + DASH package of `input` into `outDir`. Encryption is off unless both
/// `keyId` and `key` are given (32 hex digits each); `scheme` is `cenc` or `cbcs`.
#[napi(ts_return_type = "Promise<PackageCounts>")]
pub fn package(
    input: String,
    out_dir: String,
    segment_seconds: f64,
    key_id: Option<String>,
    key: Option<String>,
    scheme: Option<String>,
) -> AsyncTask<PackageTask> {
    AsyncTask::new(PackageTask {
        input,
        out_dir: out_dir.into(),
        segment_seconds,
        key_id,
        key,
        scheme: scheme.unwrap_or_else(|| "cenc".to_owned()),
    })
}
