#!/usr/bin/env python
"""Temporary debug instrumentation for the VP9 pixel-exactness hunt.
All additions are gated behind TPT_VP9_TRACE and removed by undo_trace.py."""
import io

def patch(name, old, new):
    p = f"tpt-kinetix-vp9/src/{name}"
    s = io.open(p, encoding="utf-8").read()
    assert old in s, f"pattern not found in {name}: {old[:60]!r}"
    s = s.replace(old, new, 1)
    io.open(p, "w", encoding="utf-8", newline="\n").write(s)

# 1. booldec: expose consumption
p = "tpt-kinetix-vp9/src/booldec.rs"
s = io.open(p, encoding="utf-8").read()
old = """    /// `L(n)` in the spec: n literal bits, MSB first, each with prob 128.
    pub fn read_literal(&mut self, n: u32) -> u32 {"""
new = """    /// Bits consumed so far (for debugging).
    pub fn bits_consumed(&self) -> usize {
        self.next_bit
    }

    /// `L(n)` in the spec: n literal bits, MSB first, each with prob 128.
    pub fn read_literal(&mut self, n: u32) -> u32 {"""
assert old in s
s = s.replace(old, new, 1)
io.open(p, "w", encoding="utf-8", newline="\n").write(s)

# 2. decoder.rs: print ch consumption after compressed header + tile consumption
p = "tpt-kinetix-vp9/src/decoder.rs"
s = io.open(p, encoding="utf-8").read()
old = """        let mut bc = BoolDecoder::new(ch)?;
        parse_compressed_header(&mut bc, &mut h, &mut probs, &self.frame_ctxs[c])?;"""
new = """        let mut bc = BoolDecoder::new(ch)?;
        parse_compressed_header(&mut bc, &mut h, &mut probs, &self.frame_ctxs[c])?;
        if std::env::var("TPT_VP9_TRACE").is_ok() {
            eprintln!(
                "TRACE ch: consumed {}/{} bytes",
                bc.bits_consumed() / 8,
                h.compressed_header_size
            );
        }"""
assert old in s
s = s.replace(old, new, 1)

old = """                let mut tbc = BoolDecoder::new(chunk)?;
                if tbc.read_bool(128) {
                    return Err(KinetixError::Parse("vp9: tile marker bit set".into()));
                }"""
new = """                let mut tbc = BoolDecoder::new(chunk)?;
                if tbc.read_bool(128) {
                    return Err(KinetixError::Parse("vp9: tile marker bit set".into()));
                }
                if std::env::var("TPT_VP9_TRACE").is_ok() {
                    eprintln!("TRACE tile ({tr},{tc}): {} bytes", chunk.len());
                }
                let tile_start_bits = tbc.bits_consumed();"""
assert old in s
s = s.replace(old, new, 1)

old = """                    tile.tile_row_start = row_start;
                    tile.tile_row_end = row_end;
                    tile.decode_tile(&mut tbc)?;"""
new = """                    tile.tile_row_start = row_start;
                    tile.tile_row_end = row_end;
                    tile.decode_tile(&mut tbc)?;
                if std::env::var("TPT_VP9_TRACE").is_ok() {
                    eprintln!(
                        "TRACE tile ({tr},{tc}) done: consumed {}/{} bytes",
                        (tbc.bits_consumed() - tile_start_bits) / 8,
                        chunk.len()
                    );
                }"""
assert old in s
s = s.replace(old, new, 1)
io.open(p, "w", encoding="utf-8", newline="\n").write(s)

print("trace instrumentation applied")
