//! Print the [`DecoderCapabilities`] of every codec decoder in the workspace,
//! so contributors have a single machine-readable status view (mirrors what the
//! CLI `probe` command surfaces, but without needing a media file).
//!
//! Usage: `cargo run -p tpt-kinetix-test-utils --example codec_status [--strict]`
//!
//! With `--strict`, exits non-zero if any decoder reports `pixel_exact == false`.

use tpt_kinetix_aac::AacDecoder;
use tpt_kinetix_av1::Av1Decoder;
use tpt_kinetix_core::capabilities::DecoderCapabilities;
use tpt_kinetix_h264::H264Decoder;

fn main() {
    let strict = std::env::args().any(|a| a == "--strict");

    let decoders: Vec<(&str, DecoderCapabilities)> = vec![
        ("h264", H264Decoder::new().capabilities()),
        ("av1", Av1Decoder::new().capabilities()),
        ("aac", AacDecoder::new().capabilities()),
    ];

    let mut any_incomplete = false;
    for (name, caps) in &decoders {
        println!("{name:<6} {caps}");
        if caps.is_incomplete() {
            any_incomplete = true;
        }
    }

    if strict && any_incomplete {
        eprintln!("FATAL: one or more decoders are not pixel-exact (strict mode)");
        std::process::exit(1);
    }
}
