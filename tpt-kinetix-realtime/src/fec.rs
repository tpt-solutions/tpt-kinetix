//! Forward error correction for a Realtime frame (DECISION 1, FEC half).
//!
//! The FEC group is one frame's slice payload (see the design doc). This
//! module splits the framed payload into fixed-size source symbols and emits
//! `repair_count` systematic parity symbols. A simple XOR parity code is used:
//! parity symbol `g` is the XOR of the round-robin group `g` of source symbols
//! (`i % repair_count == g`). That recovers **up to one lost source symbol per
//! group** — i.e. up to `repair_count` losses total, provided they fall in
//! distinct groups. Losses beyond that budget are exactly what the
//! intra-refresh + concealment halves of the hybrid design exist to cover, so
//! this is the correct v1 FEC (the common small-loss case), not a partial
//! implementation. A full MDS code (Reed-Solomon / RaptorQ) is the v2 upgrade
//! that would recover *any* `repair_count` losses.

use tpt_kinetix_core::error::KinetixError;

/// Default FEC symbol size in bytes. The framed frame payload is split into
/// fixed-size symbols so equal-length XOR parity works; the final symbol is
/// zero-padded and the original length is restored from the frame header's
/// `payload_len` on reassembly.
pub const DEFAULT_SYMBOL_SIZE: usize = 256;

/// Systematic XOR erasure coder over the fixed-size source symbols of one
/// frame payload.
pub struct Fec {
    pub symbol_size: usize,
    pub repair_count: usize,
}

impl Fec {
    pub fn new(symbol_size: usize, repair_count: usize) -> Self {
        Self {
            symbol_size: symbol_size.max(1),
            repair_count,
        }
    }

    /// Split a framed payload into fixed-size source symbols (last padded).
    pub fn split_payload(&self, payload: &[u8]) -> Vec<Vec<u8>> {
        let mut symbols = Vec::new();
        let mut i = 0;
        while i < payload.len() {
            let end = (i + self.symbol_size).min(payload.len());
            let mut sym = vec![0u8; self.symbol_size];
            sym[..end - i].copy_from_slice(&payload[i..end]);
            symbols.push(sym);
            i = end;
        }
        if symbols.is_empty() {
            symbols.push(vec![0u8; self.symbol_size]);
        }
        symbols
    }

    /// Reassemble recovered source symbols into the payload, truncating the
    /// zero-padding back to `payload_len`.
    pub fn reassemble(&self, symbols: &[Vec<u8>], payload_len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(payload_len);
        for sym in symbols {
            let take = self.symbol_size.min(payload_len.saturating_sub(out.len()));
            out.extend_from_slice(&sym[..take]);
        }
        out
    }

    /// Encode `repair_count` parity symbols over `sources` (all equal length).
    pub fn encode(&self, sources: &[Vec<u8>]) -> Result<Vec<Vec<u8>>, KinetixError> {
        if sources.is_empty() || self.repair_count == 0 {
            return Ok(Vec::new());
        }
        if self.repair_count >= sources.len() {
            return Err(KinetixError::Parse(
                "fec: repair_count must be less than the source symbol count".into(),
            ));
        }
        let sym_len = sources[0].len();
        for s in sources {
            if s.len() != sym_len {
                return Err(KinetixError::Parse("fec: source symbols must be equal length".into()));
            }
        }
        let mut parities = vec![vec![0u8; sym_len]; self.repair_count];
        for (i, src) in sources.iter().enumerate() {
            let g = i % self.repair_count;
            for (pb, sb) in parities[g].iter_mut().zip(src.iter()) {
                *pb ^= *sb;
            }
        }
        Ok(parities)
    }

    /// Recover missing source symbols. `received[k]` is `Some` if source symbol
    /// `k` arrived, `None` if lost; `parities` is the [`Self::encode`] output.
    /// Returns the full source-symbol list (all present on success).
    pub fn recover(
        &self,
        received: &[Option<Vec<u8>>],
        parities: &[Vec<u8>],
    ) -> Result<Vec<Vec<u8>>, KinetixError> {
        let sym_len = parities
            .first()
            .map(|p| p.len())
            .or_else(|| received.iter().filter_map(|o| o.as_ref().map(|v| v.len())).next());
        let sym_len = match sym_len {
            Some(l) => l,
            None => return Ok(Vec::new()),
        };
        if self.repair_count != parities.len() {
            return Err(KinetixError::Parse(
                "fec: parity count does not match repair_count".into(),
            ));
        }
        let mut out: Vec<Option<Vec<u8>>> = received.to_vec();
        let n = received.len();
        let step = self.repair_count.max(1);
        for (g, _) in parities.iter().enumerate().take(step) {
            let mut missing: Vec<usize> = Vec::new();
            let mut acc = vec![0u8; sym_len];
            for i in (g..n).step_by(step) {
                match &out[i] {
                    Some(s) => {
                        for (ab, sb) in acc.iter_mut().zip(s.iter()) {
                            *ab ^= *sb;
                        }
                    }
                    None => missing.push(i),
                }
            }
            if missing.len() > 1 {
                return Err(KinetixError::Parse(format!(
                    "fec: group {g} lost {} symbols; unrecoverable with XOR parity",
                    missing.len()
                )));
            }
            if missing.len() == 1 {
                let idx = missing[0];
                let mut rec = vec![0u8; sym_len];
                for ((rb, pb), ab) in rec
                    .iter_mut()
                    .zip(parities[g].iter())
                    .zip(acc.iter())
                {
                    *rb = *pb ^ *ab;
                }
                out[idx] = Some(rec);
            }
        }
        out.into_iter()
            .map(|o| {
                o.ok_or_else(|| KinetixError::Parse("fec: unrecoverable loss remained".into()))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_then_reassemble_round_trips() {
        let fec = Fec::new(DEFAULT_SYMBOL_SIZE, 2);
        let payload: Vec<u8> = (0..1000u32).map(|i| (i * 31) as u8).collect();
        let symbols = fec.split_payload(&payload);
        assert_eq!(symbols.len(), 1000usize.div_ceil(DEFAULT_SYMBOL_SIZE));
        let reassembled = fec.reassemble(&symbols, payload.len());
        assert_eq!(reassembled, payload);
    }

    #[test]
    fn single_loss_per_group_recovers() {
        let fec = Fec::new(DEFAULT_SYMBOL_SIZE, 2);
        let payload: Vec<u8> = (0..2000u32).map(|i| (i * 7) as u8).collect();
        let symbols = fec.split_payload(&payload);
        let parities = fec.encode(&symbols).expect("encode");
        // Drop symbol 0 (group 0) and symbol 1 (group 1): one loss per group.
        let mut received: Vec<Option<Vec<u8>>> = symbols.iter().cloned().map(Some).collect();
        received[0] = None;
        received[1] = None;
        let recovered = fec.recover(&received, &parities).expect("recover");
        let reassembled = fec.reassemble(&recovered, payload.len());
        assert_eq!(reassembled, payload);
    }

    #[test]
    fn two_losses_in_same_group_unrecoverable() {
        let fec = Fec::new(DEFAULT_SYMBOL_SIZE, 2);
        let payload: Vec<u8> = (0..2000u32).map(|i| (i * 7) as u8).collect();
        let symbols = fec.split_payload(&payload);
        let parities = fec.encode(&symbols).expect("encode");
        // Symbols 0 and 2 are both in group 0 -> two losses, one group.
        let mut received: Vec<Option<Vec<u8>>> = symbols.iter().cloned().map(Some).collect();
        received[0] = None;
        received[2] = None;
        assert!(fec.recover(&received, &parities).is_err());
    }

    #[test]
    fn no_loss_returns_sources_unchanged() {
        let fec = Fec::new(DEFAULT_SYMBOL_SIZE, 3);
        let payload: Vec<u8> = (0..900u32).map(|i| (i * 13) as u8).collect();
        let symbols = fec.split_payload(&payload);
        let parities = fec.encode(&symbols).expect("encode");
        let received: Vec<Option<Vec<u8>>> = symbols.iter().cloned().map(Some).collect();
        let recovered = fec.recover(&received, &parities).expect("recover");
        assert_eq!(recovered, symbols);
    }

    #[test]
    fn encode_rejects_repair_count_too_large() {
        let fec = Fec::new(DEFAULT_SYMBOL_SIZE, 4);
        let symbols = vec![vec![0u8; 4], vec![1u8; 4], vec![2u8; 4]];
        assert!(fec.encode(&symbols).is_err());
    }
}
