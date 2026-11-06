//! Common Encryption (CENC, ISO/IEC 23001-7), the `cenc` scheme: AES-128 in
//! counter mode over whole samples, with 8-byte per-sample IVs.
//!
//! Only the whole-sample form is written. That is valid for VP9, AV1 and Opus,
//! whose samples have no NAL-unit structure that a subsample map would need to
//! protect separately.

use std::fmt;

use aes::Aes128;
use ctr::cipher::{KeyIvInit, StreamCipher};

use crate::entries::{boxed, full_box};
use crate::MuxError;

type Aes128Ctr = ctr::Ctr64BE<Aes128>;

/// A content key and its identifier, shared by every protected track.
#[derive(Clone, PartialEq, Eq)]
pub struct Encryption {
    /// The key ID that players look the key up by.
    pub key_id: [u8; 16],
    /// The 128-bit content key.
    pub key: [u8; 16],
}

// The key is never printed.
impl fmt::Debug for Encryption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Encryption")
            .field("key_id", &hex(&self.key_id))
            .field("key", &"<redacted>")
            .finish()
    }
}

/// Lower-case hex of `bytes`.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Parses 32 hex digits into 16 bytes.
pub fn parse_hex16(s: &str) -> Option<[u8; 16]> {
    let s = s.trim();
    if s.len() != 32 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

/// Encrypts or decrypts `data` in place with the sample's 8-byte IV. The
/// counter starts at zero for every sample, and CTR is its own inverse.
pub fn apply_ctr(key: &[u8; 16], iv: [u8; 8], data: &mut [u8]) {
    let mut block = [0u8; 16];
    block[..8].copy_from_slice(&iv);
    let mut cipher = Aes128Ctr::new(key.into(), (&block).into());
    cipher.apply_keystream(data);
}

/// Rewrites a sample entry (`vp09`, `Opus`, ...) as its protected form (`encv`
/// or `enca`): the same body, plus a `sinf` box that names the original format
/// and carries the `tenc` defaults (protected, 8-byte IVs, the key ID).
pub fn protect_entry(entry: &[u8], audio: bool, enc: &Encryption) -> Result<Vec<u8>, MuxError> {
    if entry.len() < 8 {
        return Err(MuxError::InvalidConfig("sample entry is truncated".into()));
    }
    let original: [u8; 4] = [entry[4], entry[5], entry[6], entry[7]];
    let mut tenc_body = vec![0u8, 0, 1, 8]; // reserved, crypt/skip, isProtected, IV size
    tenc_body.extend_from_slice(&enc.key_id);
    let mut schm_body = b"cenc".to_vec();
    schm_body.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    let sinf = boxed(
        b"sinf",
        &[
            boxed(b"frma", &original),
            full_box(b"schm", 0, 0, &schm_body),
            boxed(b"schi", &full_box(b"tenc", 0, 0, &tenc_body)),
        ]
        .concat(),
    );
    let kind: &[u8; 4] = if audio { b"enca" } else { b"encv" };
    let mut body = entry[8..].to_vec();
    body.extend_from_slice(&sinf);
    Ok(boxed(kind, &body))
}

/// The `senc` box for one fragment's samples: one 8-byte IV per sample, no
/// subsample map.
pub fn senc(ivs: &[[u8; 8]]) -> Vec<u8> {
    let mut body = (ivs.len() as u32).to_be_bytes().to_vec();
    for iv in ivs {
        body.extend_from_slice(iv);
    }
    full_box(b"senc", 0, 0, &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cipher_matches_nist_sp800_38a_ctr_vector() {
        // NIST SP 800-38A, F.5.1 (CTR-AES128.Encrypt), first block. The counter
        // block is nonce f0..f7 followed by the 64-bit counter f8..ff.
        let key = parse_hex16("2b7e151628aed2a6abf7158809cf4f3c").unwrap();
        let block = parse_hex16("f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff").unwrap();
        let mut data = parse_hex16("6bc1bee22e409f96e93d7e117393172a").unwrap();
        let mut cipher = Aes128Ctr::new((&key).into(), (&block).into());
        cipher.apply_keystream(&mut data);
        assert_eq!(hex(&data), "874d6191b620e3261bef6864990db6ce");
    }

    #[test]
    fn sample_ctr_is_its_own_inverse() {
        let key = [7u8; 16];
        let original: Vec<u8> = (0..100u8).collect();
        let mut data = original.clone();
        apply_ctr(&key, [1, 2, 3, 4, 5, 6, 7, 8], &mut data);
        assert_ne!(data, original);
        apply_ctr(&key, [1, 2, 3, 4, 5, 6, 7, 8], &mut data);
        assert_eq!(data, original);
    }

    #[test]
    fn key_ids_parse_and_reject_bad_input() {
        let id = parse_hex16("00112233445566778899aabbccddeeff").unwrap();
        assert_eq!(id[0], 0x00);
        assert_eq!(id[15], 0xff);
        assert!(parse_hex16("0011").is_none());
        assert!(parse_hex16("zz112233445566778899aabbccddeeff").is_none());
    }

    #[test]
    fn nal_unit_codecs_are_refused() {
        use crate::fragmented::FragmentWriter;
        use tpt_kinetix_core::codec::CodecId;
        use tpt_kinetix_core::stream::StreamInfo;
        let mut s = StreamInfo::new(0, CodecId::H264, 90_000);
        s.extradata = vec![1, 0x64, 0, 0x1F, 0xFF, 0xE1, 0, 0, 0];
        let enc = Encryption {
            key_id: [1; 16],
            key: [2; 16],
        };
        let w = FragmentWriter::new(&[s]).unwrap();
        assert!(w.with_encryption(Some(enc)).is_err());
    }

    #[test]
    fn debug_never_prints_the_key() {
        let e = Encryption {
            key_id: [1; 16],
            key: [0xAB; 16],
        };
        let shown = format!("{e:?}");
        assert!(!shown.contains("ab"), "{shown}");
        assert!(shown.contains("redacted"));
    }
}
