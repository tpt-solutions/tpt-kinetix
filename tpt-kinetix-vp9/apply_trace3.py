#!/usr/bin/env python
"""Dump tile start state in the decoder (temporary)."""
import io

p = "tpt-kinetix-vp9/src/decoder.rs"
s = io.open(p, encoding="utf-8").read()
old = """                if std::env::var("TPT_VP9_TRACE").is_ok() {
                    eprintln!("TRACE tile ({tr},{tc}): {} bytes", chunk.len());
                }
                let tile_start_bits = tbc.bits_consumed();"""
new = """                if std::env::var("TPT_VP9_TRACE").is_ok() {
                    eprintln!(
                        "TRACE tile ({tr},{tc}): {} bytes, ch_off={} ch_size={}, first bytes {:02x}",
                        chunk.len(),
                        h.compressed_header_offset,
                        h.compressed_header_size,
                        &chunk[..chunk.len().min(6)]
                    );
                }
                let tile_start_bits = tbc.bits_consumed();"""
assert old in s
s = s.replace(old, new, 1)
io.open(p, "w", encoding="utf-8", newline="\n").write(s)
print("ok")
