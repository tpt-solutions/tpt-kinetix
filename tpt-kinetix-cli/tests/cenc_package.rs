//! CENC (ClearKey) packaging, checked against ffmpeg: each protected track,
//! concatenated as init + segments, must decode with `-decryption_key` to the
//! same frames as the clear packaging, and must not decode to them without the
//! key. Needs ffmpeg (with libvpx-vp9 and libopus); skipped otherwise.

use std::path::{Path, PathBuf};
use std::process::Command;

const KEY_ID: &str = "00112233445566778899aabbccddeeff";
const KEY: &str = "0f0e0d0c0b0a09080706050403020100";

fn available(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn tpt(args: &[&str]) {
    let status = Command::new(env!("CARGO_BIN_EXE_tpt-kinetix"))
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "tpt-kinetix {args:?} failed");
}

/// Concatenates `init-{track}.mp4` and its `seg-{track}-*.m4s` files, in order.
fn track_file(pkg: &Path, track: usize, out: &Path) -> PathBuf {
    let mut bytes = std::fs::read(pkg.join(format!("init-{track}.mp4"))).unwrap();
    let mut segs: Vec<(usize, PathBuf)> = std::fs::read_dir(pkg)
        .unwrap()
        .filter_map(|e| {
            let p = e.ok()?.path();
            let name = p.file_name()?.to_str()?.to_string();
            let n = name
                .strip_prefix(&format!("seg-{track}-"))?
                .strip_suffix(".m4s")?
                .parse::<usize>()
                .ok()?;
            Some((n, p))
        })
        .collect();
    segs.sort();
    for (_, p) in segs {
        bytes.extend(std::fs::read(p).unwrap());
    }
    std::fs::write(out, bytes).unwrap();
    out.to_path_buf()
}

/// The frames ffmpeg decodes from `file` (framemd5 lines), optionally with a key.
fn frames(file: &Path, key: Option<&str>) -> Option<Vec<String>> {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error"]);
    if let Some(k) = key {
        cmd.args(["-decryption_key", k]);
    }
    cmd.arg("-i")
        .arg(file)
        .args(["-c", "copy", "-f", "framemd5", "-"]);
    let out = cmd.output().unwrap();
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    Some(
        text.lines()
            .filter(|l| !l.starts_with('#'))
            .map(str::to_string)
            .collect(),
    )
}

#[test]
fn cenc_and_cbcs_tracks_decrypt_to_the_clear_frames() {
    if !available("ffmpeg") || !available("ffprobe") {
        eprintln!("skipping: ffmpeg/ffprobe not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt-cenc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let webm = dir.join("av.webm");
    let status = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x240:rate=25",
            "-t",
            "4",
        ])
        .args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-t",
            "4",
        ])
        .args([
            "-pix_fmt",
            "yuv420p",
            "-c:v",
            "libvpx-vp9",
            "-g",
            "25",
            "-b:v",
            "300k",
        ])
        .args(["-c:a", "libopus", "-ac", "2", "-b:a", "64k"])
        .arg(&webm)
        .status()
        .unwrap();
    assert!(status.success());
    let av = dir.join("av.mp4");
    tpt(&[
        "remux",
        webm.to_str().unwrap(),
        av.to_str().unwrap(),
        "--faststart",
    ]);

    let clear = dir.join("clear");
    for scheme in ["cenc", "cbcs"] {
        let enc = dir.join(format!("enc-{scheme}"));
        tpt(&[
            "package",
            av.to_str().unwrap(),
            clear.to_str().unwrap(),
            "--segment-seconds",
            "2",
        ]);
        tpt(&[
            "package",
            av.to_str().unwrap(),
            enc.to_str().unwrap(),
            "--segment-seconds",
            "2",
            "--cenc-key-id",
            KEY_ID,
            "--cenc-key",
            KEY,
            "--cenc-scheme",
            scheme,
        ]);

        // The protected init segment declares encv/enca, sinf and tenc.
        let init = std::fs::read(enc.join("init-0.mp4")).unwrap();
        assert!(init.windows(4).any(|w| w == b"encv"), "encv sample entry");
        assert!(init.windows(4).any(|w| w == b"tenc"), "tenc box");
        assert!(
            init.windows(4).any(|w| w == scheme.as_bytes()),
            "schm names {scheme}"
        );
        let mpd = std::fs::read_to_string(enc.join("manifest.mpd")).unwrap();
        assert!(
            mpd.contains("cenc:default_KID=\"00112233-4455-6677-8899-aabbccddeeff\""),
            "MPD names the key ID"
        );
        assert!(!std::fs::read_to_string(clear.join("manifest.mpd"))
            .unwrap()
            .contains("ContentProtection"));
        let clear_init = std::fs::read(clear.join("init-0.mp4")).unwrap();
        assert!(
            !clear_init.windows(4).any(|w| w == b"encv"),
            "clear init is not protected"
        );

        for track in [0usize, 1] {
            let c = track_file(&clear, track, &dir.join(format!("clear-{track}.mp4")));
            let e = track_file(&enc, track, &dir.join(format!("enc-{track}.mp4")));
            let want = frames(&c, None).expect("clear track decodes");
            assert!(!want.is_empty(), "track {track} has frames");
            let got = frames(&e, Some(KEY)).expect("protected track decodes with the key");
            assert_eq!(
                got, want,
                "track {track}: keyed decode matches the clear frames"
            );
            // Without the key the payload is ciphertext: ffmpeg cannot reproduce the frames.
            let without = frames(&e, None);
            assert_ne!(
                without.as_ref(),
                Some(&want),
                "track {track}: decodes without a key"
            );
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
}
