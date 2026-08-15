# tpt-kinetix-aac

AAC audio bitstream parsing for the [TPT Kinetix](https://github.com/tpt-solutions/tpt-kinetix)
media processing engine.

This is the **audio path** foundation for the engine.

## Status

- ✅ ADTS frame header parsing (`adts`)
- ✅ `AudioSpecificConfig` parsing (`config`)
- ✅ `AacDecoder` with `DecoderCapabilities` + strict mode
- ✅ PCM reconstruction for **AAC-LC** — fully native Huffman/IMDCT/TNS/PNS/stereo pipeline, no third-party codec dependency
- ⛔ HE-AAC v1/v2 (SBR/PS) and AAC-Main/Scalable — not supported

The AAC-LC decode path is **sample-exact**: `AacDecoder::decode()` reconstructs
real PCM audio (see the `ffmpeg`-gated round-trip test in
`tests/decode_pcm.rs`).

## Decode example

```rust,no_run
use tpt_kinetix_aac::AacDecoder;
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};

let mut decoder = AacDecoder::new();
let packet = Packet {
    pts: Timestamp::NONE,
    dts: Timestamp::NONE,
    data: std::fs::read("frame.adts").unwrap(), // a single ADTS frame
    stream_index: 0,
    is_key_frame: true,
};
if let Some(frame) = decoder.decode(&packet).unwrap() {
    println!("{} samples/ch @ {} Hz", frame.samples_per_channel(), frame.sample_rate);
}
```

## Parse example

```rust
use tpt_kinetix_aac::adts::AdtsHeader;

let hdr = [0xFF, 0xF1, 0x50, 0x80, 0x01, 0x7F, 0xFC];
let parsed = AdtsHeader::parse(&hdr).unwrap();
assert_eq!(parsed.sample_rate, 44_100);
assert_eq!(parsed.channels, 2);
```

## License

MIT OR Apache-2.0
