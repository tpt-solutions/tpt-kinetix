//! ISO/IEC 14496-26 AAC audio conformance — decode the MPEG-4 `al*` / `am*`
//! conformance bitstreams and compare against `ffmpeg`'s decode of the *same*
//! elementary stream (a decoder-vs-decoder check on byte-identical input, so no
//! container edit-list / encoder-delay trimming enters the picture).
//!
//! Fixtures live in `tests/fixtures/iso/` and are fetched (git-ignored) with
//! `just fetch-aac-conformance`. When the directory is absent the whole test
//! is skipped so CI without the fixtures stays green.
//!
//! `al04_44` (LC mono, pulse), `al05_44` (LC stereo), `al18_44` (LC mono)
//! decode **bit-exact**. The multichannel / channel_config-0-PCE / mismatched-PCE
//! / SSR streams are recorded as known gaps with a pinned regression ceiling —
//! see `EXPECT` below.

use std::path::{Path, PathBuf};
use tpt_kinetix_aac::AacDecoder;
use tpt_kinetix_core::{Packet, Timestamp};

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/iso")
}

fn split_adts(adts: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 7 <= adts.len() {
        if adts[i] != 0xFF || (adts[i + 1] & 0xF0) != 0xF0 {
            break;
        }
        let len = (((adts[i + 3] as usize & 0x03) << 11)
            | ((adts[i + 4] as usize) << 3)
            | ((adts[i + 5] as usize & 0xE0) >> 5))
            & 0x1FFF;
        if len == 0 || i + len > adts.len() {
            break;
        }
        out.push(adts[i..i + len].to_vec());
        i += len;
    }
    out
}

#[derive(Clone, Copy)]
enum Expect {
    /// Byte-exact against ffmpeg (allow < ~1 LSB float rounding).
    Exact,
    /// A known gap: assert the worst per-sample error does not *exceed* this
    /// many 16-bit LSBs (a regression ceiling, not a target).
    KnownGap { max_lsb: f64 },
    /// The stream uses a tool this decoder does not implement; assert it is
    /// rejected (`decode` returns `Err` on at least one frame) rather than
    /// silently producing wrong audio.
    Rejected,
}

/// `(name, expectation, one-line note)`.
const EXPECT: &[(&str, Expect, &str)] = &[
    ("al04_44", Expect::Exact, "LC mono, pulse coding"),
    ("al05_44", Expect::Exact, "LC stereo (2x SCE)"),
    ("al18_44", Expect::Exact, "LC mono, long clip"),
    (
        "am00_88",
        Expect::KnownGap { max_lsb: 30_000.0 },
        "LC — multichannel/parse gap, TODO characterise",
    ),
    (
        "al06_44",
        Expect::KnownGap { max_lsb: 25_000.0 },
        "LC 5.1 — multichannel gap",
    ),
    (
        "al07_96",
        Expect::KnownGap { max_lsb: 60_000.0 },
        "LC 5.1 @ 96 kHz — parse errors",
    ),
    (
        "am05_44",
        Expect::KnownGap { max_lsb: 30_000.0 },
        "LC multichannel — parse errors, TODO",
    ),
    (
        "al22_chCfg0PCE_44",
        Expect::KnownGap { max_lsb: 20_000.0 },
        "LC 7.1, channel_config 0 / PCE — parse errors",
    ),
    (
        "al17_44",
        Expect::KnownGap { max_lsb: 15_000.0 },
        "LC 2x SCE w/ mismatched PCE (ffmpeg also warns)",
    ),
    (
        "al15_44",
        Expect::Rejected,
        "SSR profile — gain_control_data unsupported",
    ),
];

#[test]
fn iso_aac_conformance_suite() {
    let root = fixtures_root();
    if !root.join("al05_44.adts").exists() {
        eprintln!(
            "iso_aac_conformance_suite: no fixtures under {} — run `just fetch-aac-conformance`. Skipping.",
            root.display()
        );
        return;
    }

    let mut failures = Vec::new();
    for &(name, expect, note) in EXPECT {
        let Ok(adts) = std::fs::read(root.join(format!("{name}.adts"))) else {
            eprintln!("  {name}: missing .adts, skipped");
            continue;
        };
        let refb = std::fs::read(root.join(format!("{name}.ref.f32"))).unwrap();
        let refv: Vec<f32> = refb
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();

        let mut dec = AacDecoder::new();
        let mut got: Vec<f32> = Vec::new();
        let (mut ok, mut errs) = (0u32, 0u32);
        for f in split_adts(&adts) {
            let pkt = Packet {
                pts: Timestamp::NONE,
                dts: Timestamp::NONE,
                data: f,
                stream_index: 0,
                is_key_frame: true,
            };
            match dec.decode(&pkt) {
                Ok(Some(fr)) => {
                    ok += 1;
                    for c in fr.data.chunks_exact(4) {
                        got.push(f32::from_le_bytes([c[0], c[1], c[2], c[3]]));
                    }
                }
                Ok(None) => {}
                Err(_) => errs += 1,
            }
        }

        let n = got.len().min(refv.len());
        let (mut se, mut mx) = (0.0f64, 0.0f64);
        for i in 0..n {
            let d = (got[i] - refv[i]).abs() as f64;
            se += d * d;
            mx = mx.max(d);
        }
        let rms_lsb = (se / n.max(1) as f64).sqrt() * 32768.0;
        let max_lsb = mx * 32768.0;
        let coverage = if refv.is_empty() {
            0.0
        } else {
            got.len() as f64 / refv.len() as f64
        };

        let verdict = match expect {
            Expect::Exact => {
                let good = errs == 0 && coverage > 0.999 && max_lsb < 1.0;
                if !good {
                    failures.push(format!(
                        "{name}: expected bit-exact, got max={max_lsb:.1} LSB rms={rms_lsb:.1} \
                         errs={errs} coverage={coverage:.3}"
                    ));
                }
                if good {
                    "EXACT ✓"
                } else {
                    "EXACT ✗"
                }
            }
            Expect::KnownGap { max_lsb: ceil } => {
                if max_lsb > ceil {
                    failures.push(format!(
                        "{name}: known-gap regression — max {max_lsb:.0} LSB exceeds ceiling {ceil:.0}"
                    ));
                    "GAP ✗ (regressed)"
                } else {
                    "gap (pinned)"
                }
            }
            Expect::Rejected => {
                if errs == 0 {
                    failures.push(format!(
                        "{name}: expected the decoder to reject an unsupported tool, but no frame errored"
                    ));
                    "REJECT ✗"
                } else {
                    "rejected ✓"
                }
            }
        };
        eprintln!(
            "  {name:22} {verdict:20} max={max_lsb:9.1} LSB  rms={rms_lsb:8.1} LSB  \
             frames={ok}/{}  errs={errs}   — {note}",
            ok + errs,
        );
    }

    assert!(
        failures.is_empty(),
        "ISO AAC conformance:\n{}",
        failures.join("\n")
    );
}
