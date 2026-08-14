//! Frame payload framing.
//!
//! The coded payload that follows the frame header is split into three
//! independently rANS-coded byte ranges, length-prefixed so the decoder can
//! slice them apart without relying on stream self-delimiting:
//!
//! ```text
//! [occ_len: u32 BE][occ rANS bytes]
//! [leaf_len: u32 BE][leaf-count rANS bytes]
//! [attr rANS bytes]
//! ```
//!
//! `occ` = per-node occupancy bits (geometry), `leaf` = per-leaf point counts
//! (geometry), `attr` = attribute residuals/coefficients.

use tpt_kinetix_core::error::KinetixError;

/// The three coded byte ranges split out of a frame payload.
pub type PayloadParts<'a> = (&'a [u8], &'a [u8], &'a [u8]);

/// Frame the three coded byte ranges into a single payload buffer.
pub fn frame_payload(occ: &[u8], leaf: &[u8], attr: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + occ.len() + leaf.len() + attr.len());
    out.extend_from_slice(&(occ.len() as u32).to_be_bytes());
    out.extend_from_slice(occ);
    out.extend_from_slice(&(leaf.len() as u32).to_be_bytes());
    out.extend_from_slice(leaf);
    out.extend_from_slice(attr);
    out
}

/// Split a payload buffer back into its three coded ranges.
pub fn unframe_payload(payload: &[u8]) -> Result<PayloadParts<'_>, KinetixError> {
    if payload.len() < 8 {
        return Err(KinetixError::Parse(
            "volumetric: payload too short for length prefixes".into(),
        ));
    }
    let occ_len = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]) as usize;
    let leaf_off = 4 + occ_len;
    if payload.len() < leaf_off + 4 {
        return Err(KinetixError::Parse(
            "volumetric: payload truncated before leaf-length prefix".into(),
        ));
    }
    let leaf_len = u32::from_be_bytes([
        payload[leaf_off],
        payload[leaf_off + 1],
        payload[leaf_off + 2],
        payload[leaf_off + 3],
    ]) as usize;
    let attr_off = leaf_off + 4 + leaf_len;
    if payload.len() < attr_off {
        return Err(KinetixError::Parse(
            "volumetric: payload truncated before attribute stream".into(),
        ));
    }
    let occ = &payload[4..leaf_off];
    let leaf = &payload[leaf_off + 4..attr_off];
    let attr = &payload[attr_off..];
    Ok((occ, leaf, attr))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_round_trips() {
        let occ = [1u8, 2, 3];
        let leaf = [4u8, 5];
        let attr = [6u8, 7, 8, 9];
        let framed = frame_payload(&occ, &leaf, &attr);
        let (o, l, a) = unframe_payload(&framed).expect("unframe");
        assert_eq!(o, &occ[..]);
        assert_eq!(l, &leaf[..]);
        assert_eq!(a, &attr[..]);
    }

    #[test]
    fn rejects_truncated_payload() {
        let occ = [1u8, 2, 3];
        let leaf = [4u8, 5];
        let attr = [6u8, 7, 8, 9];
        let mut framed = frame_payload(&occ, &leaf, &attr);
        // Drop everything past the first length prefix so the buffer is too
        // short to even contain the leaf-length prefix.
        framed.truncate(4);
        assert!(unframe_payload(&framed).is_err());
    }
}
