//! Decoder concealment (DECISION 1, concealment half).
//!
//! When a slice cannot be recovered by FEC (too many losses in one parity
//! group) or is missing entirely, the decoder must never stall a frame. The
//! simplest spatially-acceptable fallback is *temporal* concealment: reuse the
//! corresponding slice from the previously decoded frame. This module fills the
//! missing slice payloads from the previous frame's payloads; a real decoder
//! would additionally run spatial concealment (edge/gradient extension) on the
//! concealed region, which is the v2 upgrade.

use tpt_kinetix_core::error::KinetixError;

/// Fill missing slices from the previous frame's slices.
///
/// `current[k]` is `Some(slice)` if slice `k` was decoded or FEC-recovered,
/// `None` if still missing. `previous` holds every slice of the prior frame in
/// grid order. Returns a full payload list with every slice present. A slice
/// that is still missing AND has no previous-frame slice to conceal from is a
/// hard error (the decoder has no data for it at all).
pub fn conceal(
    current: &[Option<Vec<u8>>],
    previous: &[Vec<u8>],
) -> Result<Vec<Vec<u8>>, KinetixError> {
    if current.len() != previous.len() {
        return Err(KinetixError::Parse(format!(
            "concealment: current slice count {} != previous {}",
            current.len(),
            previous.len()
        )));
    }
    let mut out = Vec::with_capacity(current.len());
    for (k, cur) in current.iter().enumerate() {
        match cur {
            Some(s) => out.push(s.clone()),
            None => match previous.get(k) {
                Some(p) => out.push(p.clone()),
                None => {
                    return Err(KinetixError::Parse(format!(
                        "concealment: slice {k} lost and no previous frame to conceal from"
                    )))
                }
            },
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conceals_missing_slice_from_previous_frame() {
        let prev = vec![vec![1u8, 2], vec![3u8, 4], vec![5u8, 6]];
        let current: Vec<Option<Vec<u8>>> = vec![
            Some(vec![1u8, 2]),
            None, // slice 1 lost
            Some(vec![5u8, 6]),
        ];
        let out = conceal(&current, &prev).expect("conceal");
        assert_eq!(out, prev); // slice 1 falls back to previous[1]
    }

    #[test]
    fn all_present_passthrough() {
        let prev = vec![vec![1u8], vec![2u8]];
        let current: Vec<Option<Vec<u8>>> = vec![Some(vec![9u8]), Some(vec![8u8])];
        let out = conceal(&current, &prev).expect("conceal");
        assert_eq!(out, vec![vec![9u8], vec![8u8]]);
    }

    #[test]
    fn missing_with_no_previous_errors() {
        let prev: Vec<Vec<u8>> = Vec::new();
        let current: Vec<Option<Vec<u8>>> = vec![None];
        assert!(conceal(&current, &prev).is_err());
    }

    #[test]
    fn length_mismatch_errors() {
        let prev = vec![vec![1u8], vec![2u8]];
        let current: Vec<Option<Vec<u8>>> = vec![Some(vec![1u8])];
        assert!(conceal(&current, &prev).is_err());
    }
}
