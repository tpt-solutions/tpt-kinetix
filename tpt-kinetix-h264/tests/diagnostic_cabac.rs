//! Scratch diagnostic for debugging the CABAC I-slice conformance failure.
//! Not part of the permanent test suite's correctness gate.

use std::process::Command;

use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::H264Decoder;

fn run(cmd: &mut Command) -> bool {
    cmd.output().map(|o| o.status.success()).unwrap_or(false)
}

#[test]
fn debug_cabac_strict_error() {
    let dir = std::env::temp_dir().join("tpt_kinetix_h264_cabac_conformance");
    std::fs::create_dir_all(&dir).unwrap();
    let h264 = dir.join("cabac_nodblk.h264");
    if !h264.exists() {
        let input_spec = "testsrc=size=64x48:rate=1:duration=1".to_string();
        let ok = run(Command::new("ffmpeg").args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            &input_spec,
            "-frames:v",
            "1",
            "-c:v",
            "libx264",
            "-profile:v",
            "main",
            "-g",
            "1",
            "-bf",
            "0",
            "-pix_fmt",
            "yuv420p",
            "-x264-params",
            "cabac=1:ref=1:bframes=0:8x8dct=0:weightp=0:aud=0:no-deblock=1",
            h264.to_str().unwrap(),
        ]));
        assert!(ok, "ffmpeg encode failed");
    }

    let annexb = std::fs::read(&h264).unwrap();
    let mut dec = H264Decoder::new();
    dec.set_strict(true);
    let pkt = Packet {
        pts: Timestamp::new(0, (1, 30)),
        dts: Timestamp::new(0, (1, 30)),
        data: annexb,
        stream_index: 0,
        is_key_frame: true,
    };
    let result = dec.decode(&pkt);
    eprintln!("strict decode result: {result:?}");
}

#[test]
fn debug_cabac_flat_strict_error() {
    let h264 = std::path::Path::new(r"C:\Users\phill\AppData\Local\Temp\flat_cabac.h264");
    let annexb = std::fs::read(h264).unwrap();
    let mut dec = H264Decoder::new();
    dec.set_strict(true);
    let pkt = Packet {
        pts: Timestamp::new(0, (1, 30)),
        dts: Timestamp::new(0, (1, 30)),
        data: annexb,
        stream_index: 0,
        is_key_frame: true,
    };
    let result = dec.decode(&pkt);
    eprintln!("flat strict decode result: {result:?}");
}

#[test]
fn debug_cabac_gradient_strict_error() {
    let h264 = std::path::Path::new(r"C:\Users\phill\AppData\Local\Temp\grad_cabac.h264");
    let annexb = std::fs::read(h264).unwrap();
    let mut dec = H264Decoder::new();
    dec.set_strict(true);
    let pkt = Packet {
        pts: Timestamp::new(0, (1, 30)),
        dts: Timestamp::new(0, (1, 30)),
        data: annexb,
        stream_index: 0,
        is_key_frame: true,
    };
    let result = dec.decode(&pkt);
    eprintln!("gradient strict decode result: {:?}", result.map(|r| r.is_some()));
}

#[test]
fn debug_cabac_test32_strict_error() {
    let h264 = std::path::Path::new(r"C:\Users\phill\AppData\Local\Temp\test32_cabac.h264");
    let annexb = std::fs::read(h264).unwrap();
    let mut dec = H264Decoder::new();
    dec.set_strict(true);
    let pkt = Packet {
        pts: Timestamp::new(0, (1, 30)),
        dts: Timestamp::new(0, (1, 30)),
        data: annexb,
        stream_index: 0,
        is_key_frame: true,
    };
    let result = dec.decode(&pkt);
    eprintln!("test32 strict decode result: {:?}", result.map(|r| r.is_some()));
}

#[test]
fn debug_cabac_test16_strict_error() {
    let h264 = std::path::Path::new(r"C:\Users\phill\AppData\Local\Temp\test16_cabac.h264");
    let annexb = std::fs::read(h264).unwrap();
    let mut dec = H264Decoder::new();
    dec.set_strict(true);
    let pkt = Packet {
        pts: Timestamp::new(0, (1, 30)),
        dts: Timestamp::new(0, (1, 30)),
        data: annexb,
        stream_index: 0,
        is_key_frame: true,
    };
    let result = dec.decode(&pkt);
    eprintln!("test16 strict decode result: {:?}", result.map(|r| r.is_some()));
}

#[test]
fn debug_cabac_test16_pixel_compare() {
    let h264 = std::path::Path::new(r"C:\Users\phill\AppData\Local\Temp\test16_cabac.h264");
    let refyuv = std::path::Path::new(r"C:\Users\phill\AppData\Local\Temp\test16_ref.yuv");
    let annexb = std::fs::read(h264).unwrap();
    let refbytes = std::fs::read(refyuv).unwrap();

    let mut dec = H264Decoder::new();
    // Not strict: parse_i_slice_cabac's Err currently falls back silently,
    // but we've relaxed the end_of_slice hard-error to a warning so we can
    // inspect the (possibly wrong) decoded pixels directly.
    let pkt = Packet {
        pts: Timestamp::new(0, (1, 30)),
        dts: Timestamp::new(0, (1, 30)),
        data: annexb,
        stream_index: 0,
        is_key_frame: true,
    };
    let frame = dec.decode(&pkt).unwrap().unwrap();
    eprintln!("frame.data.len()={} ref.len()={}", frame.data.len(), refbytes.len());
    eprintln!("ours luma: {:?}", &frame.data[0..256]);
    eprintln!("ref  luma: {:?}", &refbytes[0..256]);
    let mut first_diff = None;
    for i in 0..frame.data.len().min(refbytes.len()) {
        if frame.data[i] != refbytes[i] && first_diff.is_none() {
            first_diff = Some(i);
        }
    }
    eprintln!("first_diff_index={first_diff:?}");
}

#[test]
fn debug_cabac_test16_hiqp_strict_error() {
    let h264 = std::path::Path::new(r"C:\Users\phill\AppData\Local\Temp\test16_hiqp.h264");
    let annexb = std::fs::read(h264).unwrap();
    let mut dec = H264Decoder::new();
    dec.set_strict(true);
    let pkt = Packet {
        pts: Timestamp::new(0, (1, 30)),
        dts: Timestamp::new(0, (1, 30)),
        data: annexb,
        stream_index: 0,
        is_key_frame: true,
    };
    let result = dec.decode(&pkt);
    eprintln!("test16 hiqp strict decode result: {:?}", result.map(|r| r.is_some()));
}
