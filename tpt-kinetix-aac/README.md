# tpt-kinetix-aac

AAC audio bitstream parsing for the [TPT Kinetix](https://github.com/tpt-solutions/tpt-kinetix)
media processing engine.

This is the **audio path** foundation for the engine.

## Status

- ✅ ADTS frame header parsing (`adts`)
- ✅ `AudioSpecificConfig` parsing (`config`)
- ✅ `AacDecoder` shell with `DecoderCapabilities` + strict mode
- ⛔ PCM reconstruction (MDCT / Huffman spectral decode / TNS / SBR / PS) — not
  implemented

The decoder is **not sample-exact**: it parses framing only. See
[`docs/codec-evaluations/aac.md`](../docs/codec-evaluations/aac.md) for the
recommended path to correct PCM output (wrapping `symphonia-codec-aac`) versus a
KG-generated native decoder.

## Example

```rust
use tpt_kinetix_aac::adts::AdtsHeader;

let hdr = [0xFF, 0xF1, 0x50, 0x80, 0x01, 0x7F, 0xFC];
let parsed = AdtsHeader::parse(&hdr).unwrap();
assert_eq!(parsed.sample_rate, 44_100);
assert_eq!(parsed.channels, 2);
```

## License

MIT OR Apache-2.0
