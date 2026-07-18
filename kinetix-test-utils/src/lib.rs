//! `kinetix-test-utils` — shared testing helpers for the TPT Kinetix workspace.
//!
//! This is an **internal dev/test crate** (not published to crates.io). It
//! provides:
//!
//! - [`pixel_diff`] — PSNR and tolerance helpers for comparing decoded frames
//! - [`reference`] — drive external reference decoders (`ffmpeg`, `dav1d`) and
//!   diff their output against Kinetix decoders, skipping gracefully when the
//!   binaries are unavailable
//! - [`synthetic`] — generate synthetic frames and minimal bitstreams
//! - [`corpus`] — reusable malformed-input corpora for fuzz-regression tests

pub mod corpus;
pub mod pixel_diff;
pub mod reference;
pub mod synthetic;
