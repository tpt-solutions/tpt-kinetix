//! Python bindings for the TPT Kinetix container tools.
//!
//! The functions here are the Rust side of the `tpt_kinetix` package; see
//! `python/tpt_kinetix/__init__.py` for the public names. Every failure is raised
//! as [`KinetixError`], carrying the Rust error chain as its message.

use std::path::Path;

use pyo3::create_exception;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::types::PyDict;

create_exception!(
    _tpt_kinetix,
    KinetixError,
    PyException,
    "A TPT Kinetix operation failed: the file could not be read, written or packaged."
);

fn fail(e: impl std::fmt::Display) -> PyErr {
    KinetixError::new_err(e.to_string())
}

/// The library version, e.g. `0.1.2`.
#[pyfunction]
fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Lists the tracks of a WebM, Matroska, IVF or MP4 file.
///
/// Returns a dict with `format` (a string) and `streams` (a list of dicts with
/// `index`, `codec`, `type`, and `width`/`height` for video or `channels`/
/// `sample_rate` for audio).
#[pyfunction]
fn probe<'py>(py: Python<'py>, path: &str) -> PyResult<Bound<'py, PyAny>> {
    let json = tpt_kinetix_ops::summary::summary_json(Path::new(path)).map_err(fail)?;
    py.import("json")?.call_method1("loads", (json,))
}

/// Writes a copy of `input` with the MP4 index moved to the front.
#[pyfunction]
fn faststart(input: &str, output: &str) -> PyResult<()> {
    tpt_kinetix_ops::faststart_file(Path::new(input), Path::new(output)).map_err(fail)
}

/// Copies the streams of `input` into `output` without decoding.
///
/// The container follows the extension of `output`: `.webm` or `.mkv` writes WebM,
/// anything else writes MP4. `faststart` moves the MP4 index to the front.
/// `fragment_ms` writes fragmented MP4 with that target fragment length.
#[pyfunction]
#[pyo3(signature = (input, output, *, faststart=false, fragment_ms=None))]
fn remux(input: &str, output: &str, faststart: bool, fragment_ms: Option<u32>) -> PyResult<()> {
    tpt_kinetix_ops::remux::remux(input, Path::new(output), faststart, fragment_ms).map_err(fail)
}

/// Writes an HLS + DASH package of `input` into `out_dir` and returns its counts as
/// a dict (`tracks`, `segments`, `files`, `bytes`).
///
/// Encryption is off unless both `key_id` and `key` are given (32 hex digits each).
/// `scheme` is `"cenc"` or `"cbcs"`. CENC only covers VP9, AV1 and Opus samples.
#[pyfunction]
#[pyo3(signature = (input, out_dir, *, segment_seconds=6.0, key_id=None, key=None, scheme="cenc"))]
fn package<'py>(
    py: Python<'py>,
    input: &str,
    out_dir: &str,
    segment_seconds: f64,
    key_id: Option<&str>,
    key: Option<&str>,
    scheme: &str,
) -> PyResult<Bound<'py, PyDict>> {
    let encryption = tpt_kinetix_ops::encryption_from_hex(key_id, key, scheme).map_err(fail)?;
    let done =
        tpt_kinetix_ops::package::package(input, Path::new(out_dir), segment_seconds, encryption)
            .map_err(fail)?;
    let out = PyDict::new(py);
    out.set_item("tracks", done.tracks)?;
    out.set_item("segments", done.segments)?;
    out.set_item("files", done.files)?;
    out.set_item("bytes", done.bytes)?;
    Ok(out)
}

#[pymodule]
fn _tpt_kinetix(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("KinetixError", m.py().get_type::<KinetixError>())?;
    m.add_function(wrap_pyfunction!(version, m)?)?;
    m.add_function(wrap_pyfunction!(probe, m)?)?;
    m.add_function(wrap_pyfunction!(faststart, m)?)?;
    m.add_function(wrap_pyfunction!(remux, m)?)?;
    m.add_function(wrap_pyfunction!(package, m)?)?;
    Ok(())
}
