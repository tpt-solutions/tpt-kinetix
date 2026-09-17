#!/usr/bin/env python
"""Per-block bool-decoder consumption trace (temporary)."""
import io

p = "tpt-kinetix-vp9/src/frame.rs"
s = io.open(p, encoding="utf-8").read()

# decode_block needs access to the BC to report consumption: pass it in and
# print at block end.
old = """    fn decode_block(
        &mut self,
        bc: &mut BoolDecoder,
        row: usize,
        col: usize,
        bl: usize,
        bp: usize,
    ) -> Result<(), KinetixError> {
        self.row = row;"""
new = """    fn decode_block(
        &mut self,
        bc: &mut BoolDecoder,
        row: usize,
        col: usize,
        bl: usize,
        bp: usize,
    ) -> Result<(), KinetixError> {
        let trace_start_bits = bc.bits_consumed();
        self.row = row;"""
assert old in s
s = s.replace(old, new, 1)

old = """        // left/above MV cache update (inter frames only)
        if self.hdr.frame_type != FrameType::Key && !self.hdr.intra_only {"""
new = """        if std::env::var("TPT_VP9_TRACE").is_ok() {
            eprintln!(
                "TRACE block r{row} c{col} bs={bs} skip={} intra={} modes={:?} uv={} tx={} seg={} bytes={}..{}",
                self.b.skip,
                self.b.intra,
                self.b.mode,
                self.b.uvmode,
                self.b.tx,
                self.b.seg_id,
                trace_start_bits / 8,
                bc.bits_consumed() / 8
            );
        }

        // left/above MV cache update (inter frames only)
        if self.hdr.frame_type != FrameType::Key && !self.hdr.intra_only {"""
assert old in s
s = s.replace(old, new, 1)
io.open(p, "w", encoding="utf-8", newline="\n").write(s)
print("ok")
