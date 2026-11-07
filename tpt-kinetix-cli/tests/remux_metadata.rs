//! Metadata (`moov/udta`: titles, artists, ...) survives an MP4 to MP4 remux, byte
//! for byte as far as ffprobe can tell, and is not copied by `sanitize`. The fixtures
//! are made with ffmpeg and checked with ffprobe. Skipped without both tools.

use std::path::Path;
use std::process::Command;

fn available(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn ffmpeg(args: &[&str]) {
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y"])
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "ffmpeg {args:?} failed");
}

fn tpt(args: &[&str]) {
    let status = Command::new(env!("CARGO_BIN_EXE_tpt-kinetix"))
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "tpt-kinetix {args:?} failed");
}

/// The `title` tag ffprobe reports for `path`, or `None`.
fn title_of(path: &Path) -> Option<String> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format_tags=title",
            "-of",
            "default=nw=1:nk=1",
        ])
        .arg(path)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

#[test]
fn mp4_remux_keeps_the_title_and_sanitize_drops_it() {
    if !available("ffmpeg") || !available("ffprobe") {
        eprintln!("skipping: ffmpeg/ffprobe not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt-remux-meta-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc=size=160x120:rate=10",
        "-t",
        "1",
        "-c:v",
        "libvpx-vp9",
        "-b:v",
        "150k",
        "-metadata",
        "title=Kinetix metadata check",
        "-metadata",
        "artist=TPT",
        src.to_str().unwrap(),
    ]);
    assert_eq!(
        title_of(&src).as_deref(),
        Some("Kinetix metadata check"),
        "the fixture carries a title"
    );

    let out = dir.join("remux.mp4");
    tpt(&["remux", src.to_str().unwrap(), out.to_str().unwrap()]);
    assert_eq!(
        title_of(&out).as_deref(),
        Some("Kinetix metadata check"),
        "remux keeps the title"
    );

    // The fragmented output carries the metadata in its init segment's moov.
    let frag = dir.join("frag.mp4");
    tpt(&[
        "remux",
        src.to_str().unwrap(),
        frag.to_str().unwrap(),
        "--fragmented",
    ]);
    assert!(frag.metadata().unwrap().len() > 0);
    assert_eq!(
        title_of(&frag).as_deref(),
        Some("Kinetix metadata check"),
        "fragmented remux keeps the title"
    );

    // WebM output carries the same text tags, as a Matroska Tags element.
    let webm = dir.join("out.webm");
    tpt(&["remux", src.to_str().unwrap(), webm.to_str().unwrap()]);
    assert_eq!(
        title_of(&webm).as_deref(),
        Some("Kinetix metadata check"),
        "WebM remux keeps the title as a tag"
    );

    // Chapters: an MP4 with two chapters keeps them, in order, in WebM output.
    let chapter_text = dir.join("chapters.txt");
    std::fs::write(
        &chapter_text,
        ";FFMETADATA1
[CHAPTER]
TIMEBASE=1/1000
START=0
END=500
title=Intro
[CHAPTER]
TIMEBASE=1/1000
START=500
END=1000
title=Main part
",
    )
    .unwrap();
    let chaptered = dir.join("chaptered.mp4");
    ffmpeg(&[
        "-i",
        src.to_str().unwrap(),
        "-i",
        chapter_text.to_str().unwrap(),
        "-map_metadata",
        "1",
        "-map_chapters",
        "1",
        "-c",
        "copy",
        chaptered.to_str().unwrap(),
    ]);
    let chapter_webm = dir.join("chaptered.webm");
    tpt(&[
        "remux",
        chaptered.to_str().unwrap(),
        chapter_webm.to_str().unwrap(),
    ]);
    let titles = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "chapter_tags=title",
            "-of",
            "csv=p=0",
        ])
        .arg(&chapter_webm)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&titles.stdout)
            .lines()
            .collect::<Vec<_>>(),
        vec!["Intro", "Main part"],
        "WebM output keeps the chapters"
    );

    // Matroska input: the tags and chapters of an MKV carry over to WebM output too.
    let mkv = dir.join("with-meta.mkv");
    ffmpeg(&[
        "-i",
        src.to_str().unwrap(),
        "-i",
        chapter_text.to_str().unwrap(),
        "-map_metadata",
        "0",
        "-map_chapters",
        "1",
        "-c",
        "copy",
        mkv.to_str().unwrap(),
    ]);
    let from_mkv = dir.join("from-mkv.webm");
    tpt(&["remux", mkv.to_str().unwrap(), from_mkv.to_str().unwrap()]);
    assert_eq!(
        title_of(&from_mkv).as_deref(),
        Some("Kinetix metadata check"),
        "a Matroska input's title carries over to WebM"
    );
    let mkv_titles = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "chapter_tags=title",
            "-of",
            "csv=p=0",
        ])
        .arg(&from_mkv)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&mkv_titles.stdout)
            .lines()
            .collect::<Vec<_>>(),
        vec!["Intro", "Main part"],
        "a Matroska input's chapters carry over to WebM"
    );

    let clean = dir.join("clean.mp4");
    tpt(&["sanitize", src.to_str().unwrap(), clean.to_str().unwrap()]);
    assert_eq!(title_of(&clean), None, "sanitize does not copy metadata");

    let _ = std::fs::remove_dir_all(&dir);
}
