//! Container operations shared by the `tpt-kinetix` CLI and the language bindings
//! (`tpt-kinetix-ffi`, `tpt-kinetix-py`, `tpt-kinetix-node`).
//!
//! * [`probe_json`]: the ffprobe-shaped JSON of `tpt-kinetix probe --json`.
//! * [`summary`]: the compact track list the C ABI and bindings return.
//! * [`fn@remux`] and [`sanitize`]: copy streams into a new container without decoding.
//! * [`fn@package`]: write an on-demand HLS/DASH package of a file to a directory.
//!
//! Progress is reported through `tracing` (`info` for the summaries), never on
//! stdout, so a library caller decides where it goes.

pub mod package;
pub mod probe_json;
pub mod remux;
pub mod summary;

pub use package::{encryption_from_hex, package, PackageSummary};
pub use remux::{faststart_file, remux, sanitize, Limits};
