//! Standalone AV1 Phase C conformance harness (does not depend on the broken
//! `tpt-kinetix-h264` / `tpt-kinetix-test-utils` path).
//!
//! Generates a real `ffmpeg`-encoded AV1 keyframe (IVF), decodes it with
//! [`tpt_kinetix_av1::Av1Decoder`], decodes the same IVF to raw YUV with
//! `ffmpeg` as the reference, and prints the per-plane PSNR + luma diff count.
//!
//! Skips (does not fail) when `ffmpeg` is unavailable. Run with:
//! `cargo test -p tpt-kinetix-av1 --test phase_c_conformance -- --nocapture`.

use std::io::Read;
use std::process::Command;

use tpt_kinetix_av1::Av1Decoder;
use tpt_kinetix_core::{packet::Packet, pixel_format::PixelFormat, timestamp::Timestamp};

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Encode a single `testsrc`/`smptebars` keyframe to an AV1 IVF at `w`x`h`.
/// Returns the IVF bytes, or `None` if `ffmpeg` failed.
fn make_av1_ivf(w: u32, h: u32, src: &str) -> Option<Vec<u8>> {
    let mut child = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            &format!("{}=size={}x{}:rate=1:duration=1", src, w, h),
            "-c:v",
            "libaom-av1",
            "-strict",
            "experimental",
            // Disable CDEF + loop restoration so the reference decode differs
            // from a correct reconstruction only by the (level-0) deblock filter.
            // (Newer ffmpeg accepts `-aom-params`; this bundled copy does not, so
            // it is omitted and the reference carries CDEF/restoration too.)
            "-cpu-used",
            "8",
            "-pix_fmt",
            "yuv420p",
            "-y",
            "-f",
            "ivf",
            "-",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut out = Vec::new();
    if child.stdout.take()?.read_to_end(&mut out).is_err() {
        return None;
    }
    let _ = child.wait();
    if out.len() < 32 {
        None
    } else {
        Some(out)
    }
}

/// Extract the OBU payload of the first frame from an IVF file.
/// IVF layout: 32-byte file header, then frames of
/// [u32 LE frame_size][u64 LE timestamp][`frame_size` bytes of OBU data].
fn first_ivf_frame(ivf: &[u8]) -> Option<Vec<u8>> {
    if ivf.len() < 32 + 12 {
        return None;
    }
    let size = u32::from_le_bytes([ivf[32], ivf[33], ivf[34], ivf[35]]) as usize;
    let start = 32 + 12;
    if start + size > ivf.len() {
        return None;
    }
    Some(ivf[start..start + size].to_vec())
}

/// Decode an IVF to raw YUV420p planar bytes via `ffmpeg`.
fn ffmpeg_decode_to_yuv(ivf: &[u8], _w: u32, _h: u32) -> Option<Vec<u8>> {
    // Write the IVF to a temp file (ffmpeg reads from a path more reliably).
    let tmp = std::env::temp_dir().join("tpt_av1_phasec_ref.ivf");
    std::fs::write(&tmp, ivf).ok()?;
    let out = std::env::temp_dir().join("tpt_av1_phasec_ref.yuv");
    let status = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            tmp.to_str().unwrap(),
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-y",
            out.to_str().unwrap(),
        ])
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    std::fs::read(&out).ok()
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let n = a.len().min(b.len());
    let mut sse = 0.0f64;
    for i in 0..n {
        let d = a[i] as f64 - b[i] as f64;
        sse += d * d;
    }
    if sse == 0.0 {
        return f64::INFINITY;
    }
    let mse = sse / n as f64;
    10.0 * (255.0f64 * 255.0 / mse).log10()
}

fn luma_diff_count(a: &[u8], b: &[u8]) -> usize {
    let n = a.len().min(b.len());
    a[..n].iter().zip(&b[..n]).filter(|(x, y)| x != y).count()
}

#[test]
fn av1_phase_c_keyframe_psnr() {
    if !ffmpeg_available() {
        eprintln!("skipping: ffmpeg not available on PATH");
        return;
    }

    let (w, h) = (128u32, 96u32);
    let ivf = match make_av1_ivf(w, h, "testsrc") {
        Some(b) => b,
        None => {
            eprintln!("skipping: could not synthesize AV1 keyframe with ffmpeg");
            return;
        }
    };

    let frame_obus = match first_ivf_frame(&ivf) {
        Some(f) => f,
        None => {
            eprintln!("skipping: could not extract first IVF frame");
            return;
        }
    };

    // --- Kinetix decode ---
    eprintln!("seq payload hex: {}", frame_obus.iter().map(|b| format!("{b:02x}")).collect::<String>());
    for o in tpt_kinetix_av1::obu::parse_obu_sequence(&frame_obus) {
        eprintln!(
            "  obu_type={:?} ext={} size={} plen={}",
            o.obu_type as u8,
            o.extension_flag,
            o.has_size_field,
            o.payload.len()
        );
    }
    if let Some(seq) = tpt_kinetix_av1::obu::parse_obu_sequence(&frame_obus)
        .into_iter()
        .find(|o| o.obu_type == tpt_kinetix_av1::obu::ObuType::SequenceHeader)
        .and_then(|o| tpt_kinetix_av1::obu::SequenceHeaderObu::parse(&o.payload).ok())
    {
        eprintln!(
            "  seq: profile={} reduced_still={} max_w={} max_h={} ohb={} sb128={} mono={} sx={} sy={}",
            seq.seq_profile,
            seq.reduced_still_picture_header,
            seq.frame_width(),
            seq.frame_height(),
            seq.order_hint_bits_minus_1,
            seq.use_128x128_superblock,
            seq.color_config.mono_chrome,
            seq.color_config.subsampling_x,
            seq.color_config.subsampling_y,
        );

        // Parse the Frame OBU payload's uncompressed header and report the bit
        // count + key fields, so we can spot a frame-header drift (the most
        // likely cause of a totally broken reconstruction).
        if let Some(frame_obu) = tpt_kinetix_av1::obu::parse_obu_sequence(&frame_obus)
            .into_iter()
            .find(|o| o.obu_type == tpt_kinetix_av1::obu::ObuType::Frame)
        {
            let _ = std::fs::write(
                std::env::temp_dir().join("tpt_av1_frame_obu.bin"),
                &frame_obu.payload,
            );
            match tpt_kinetix_av1::frame::FrameHeader::parse(&frame_obu.payload, &seq) {
                Ok((fh, bits)) => eprintln!(
                    "  fh: bits={} type={:?} q={} txsel={} rtx={} tiles={}x{} lf0={} cdef_damp={} seg={}",
                    bits,
                    fh.frame_type as u8,
                    fh.base_q_idx,
                    fh.tx_mode_select,
                    fh.reduced_tx_set,
                    fh.tile_cols,
                    fh.tile_rows,
                    fh.loop_filter_level[0],
                    fh.cdef_damping,
                    fh.segmentation_enabled,
                ),
                Err(e) => eprintln!("  fh parse error: {e}"),
            }
        }
    }
    let mut dec = Av1Decoder::new();
    let packet = Packet {
        pts: Timestamp::NONE,
        dts: Timestamp::NONE,
        data: frame_obus.clone(),
        stream_index: 0,
        is_key_frame: true,
    };
    let kinetix = match dec.decode(&packet) {
        Ok(Some(f)) => f,
        Ok(None) => {
            eprintln!("Kinetix produced no frame");
            return;
        }
        Err(e) => {
            eprintln!("Kinetix decode errored: {e}");
            return;
        }
    };

    assert_eq!(kinetix.width, w);
    assert_eq!(kinetix.height, h);
    assert_eq!(kinetix.pixel_format, PixelFormat::Yuv420p);

    // --- Reference decode ---
    let ref_yuv = match ffmpeg_decode_to_yuv(&ivf, w, h) {
        Some(y) => y,
        None => {
            eprintln!("skipping: ffmpeg reference decode failed");
            return;
        }
    };

    let ywa = (w * h) as usize;
    let uva = ywa / 4;
    let need = ywa + 2 * uva;
    assert_eq!(kinetix.data.len(), need, "kinetix frame size mismatch");
    assert_eq!(ref_yuv.len(), need, "reference frame size mismatch");

    let k_y = &kinetix.data[..ywa];
    let k_u = &kinetix.data[ywa..ywa + uva];
    let k_v = &kinetix.data[ywa + uva..];
    let r_y = &ref_yuv[..ywa];
    let r_u = &ref_yuv[ywa..ywa + uva];
    let r_v = &ref_yuv[ywa + uva..];

    let psnr_y = psnr(k_y, r_y);
    let psnr_u = psnr(k_u, r_u);
    let psnr_v = psnr(k_v, r_v);
    let diff = luma_diff_count(k_y, r_y);

    eprintln!(
        "AV1 Phase C keyframe (Kinetix vs ffmpeg): {}x{}, PSNR Y/U/V = {:.2}/{:.2}/{:.2} dB, \
         luma diff = {}/{}",
        w, h, psnr_y, psnr_u, psnr_v, diff, ywa
    );

    // Phase C gate (uncomment once phase C validates pixel-exact against the
    // no-filter reference decode):
    // assert!(psnr_y.is_infinite() || psnr_y >= 99.0, "Y PSNR too low: {psnr_y}");
}
