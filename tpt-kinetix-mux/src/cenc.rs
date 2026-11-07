//! Common Encryption (CENC, ISO/IEC 23001-7) over whole samples.
//!
//! Two schemes are written:
//!
//! - `cenc`: AES-128 in counter mode, 8-byte per-sample IVs.
//! - `cbcs`: AES-128 in CBC mode, 16-byte per-sample IVs, with no pattern
//!   (every complete 16-byte block is encrypted; a trailing partial block stays
//!   clear, as the spec requires).
//!
//! Only the whole-sample form is written. That is valid for VP9, AV1 and Opus,
//! whose samples have no NAL-unit structure that a subsample map would need to
//! protect separately.

use std::fmt;

use aes::cipher::{Block, BlockCipherEncrypt, KeyInit, KeyIvInit, StreamCipher};
use aes::Aes128;

use crate::entries::{boxed, full_box};
use crate::MuxError;

type Aes128Ctr = ctr::Ctr64BE<Aes128>;

/// Encrypts one 16-byte block in place.
fn encrypt_block_in_place(cipher: &Aes128, block: &mut [u8]) {
    let mut b = Block::<Aes128>::default();
    b.copy_from_slice(block);
    cipher.encrypt_block(&mut b);
    block.copy_from_slice(&b);
}

/// The pattern of `cbcs-pattern`: encrypt 1 block, skip 9.
const PATTERN_CRYPT: usize = 1;
const PATTERN_SKIP: usize = 9;

/// The CENC protection scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Scheme {
    /// AES-128-CTR, 8-byte IVs (the `cenc` scheme).
    #[default]
    Cenc,
    /// AES-128-CBC, 16-byte IVs (the `cbcs` scheme, no pattern).
    Cbcs,
    /// The `cbcs` scheme with the 1:9 pattern: the first 16-byte block of every
    /// ten is encrypted and nine are left clear, which is what HLS and most
    /// players expect for video.
    CbcsPattern,
}

impl Scheme {
    /// Parses `cenc` or `cbcs` (case-insensitive).
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "cenc" => Some(Scheme::Cenc),
            "cbcs" => Some(Scheme::Cbcs),
            "cbcs-pattern" => Some(Scheme::CbcsPattern),
            _ => None,
        }
    }

    /// The four-character scheme type written to `schm`.
    pub fn fourcc(self) -> [u8; 4] {
        match self {
            Scheme::Cenc => *b"cenc",
            Scheme::Cbcs | Scheme::CbcsPattern => *b"cbcs",
        }
    }

    /// Length in bytes of the per-sample IV.
    pub fn iv_len(self) -> usize {
        match self {
            Scheme::Cenc => 8,
            Scheme::Cbcs | Scheme::CbcsPattern => 16,
        }
    }
}

/// A content key, its identifier and the scheme, shared by every protected track.
#[derive(Clone, PartialEq, Eq)]
pub struct Encryption {
    /// The key ID that players look the key up by.
    pub key_id: [u8; 16],
    /// The 128-bit content key.
    pub key: [u8; 16],
    /// The protection scheme.
    pub scheme: Scheme,
}

// The key is never printed.
impl fmt::Debug for Encryption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Encryption")
            .field("key_id", &hex(&self.key_id))
            .field("scheme", &self.scheme)
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

/// Encrypts the complete 16-byte blocks of `data` in place with AES-CBC. A
/// trailing partial block is left clear.
pub fn apply_cbc_encrypt(key: &[u8; 16], iv: [u8; 16], data: &mut [u8]) {
    let cipher = Aes128::new(key.into());
    let mut prev = iv;
    for block in data.chunks_exact_mut(16) {
        for (b, p) in block.iter_mut().zip(prev.iter()) {
            *b ^= p;
        }
        encrypt_block_in_place(&cipher, block);
        prev.copy_from_slice(block);
    }
}

/// Encrypts the pattern blocks of `data` in place with AES-CBC: of every
/// `crypt + skip` complete 16-byte blocks, the first `crypt` are encrypted and the
/// rest are left clear. The chain runs over the encrypted blocks only, starting from
/// `iv`, so a skipped block does not feed the next one.
pub fn apply_cbc_pattern(key: &[u8; 16], iv: [u8; 16], data: &mut [u8], crypt: usize, skip: usize) {
    let cipher = Aes128::new(key.into());
    let mut prev = iv;
    let period = crypt + skip;
    for (i, block) in data.chunks_exact_mut(16).enumerate() {
        if i % period >= crypt {
            continue;
        }
        for (b, p) in block.iter_mut().zip(prev.iter()) {
            *b ^= p;
        }
        encrypt_block_in_place(&cipher, block);
        prev.copy_from_slice(block);
    }
}

/// The per-sample CBC IV for a counter value: the AES encryption of the counter
/// block, so the IV is unique for each sample and not predictable without the key.
pub fn cbc_iv(key: &[u8; 16], counter: u64) -> [u8; 16] {
    let cipher = Aes128::new(key.into());
    let mut block = [0u8; 16];
    block[8..].copy_from_slice(&counter.to_be_bytes());
    encrypt_block_in_place(&cipher, &mut block);
    block
}

/// Protects one sample in place under `enc` for the counter value `counter`, and
/// returns the IV bytes that the `senc` box must carry.
pub fn protect_sample(enc: &Encryption, counter: u64, data: &mut [u8]) -> Vec<u8> {
    match enc.scheme {
        Scheme::Cenc => {
            let iv = counter.to_be_bytes();
            apply_ctr(&enc.key, iv, data);
            iv.to_vec()
        }
        Scheme::Cbcs => {
            let iv = cbc_iv(&enc.key, counter);
            apply_cbc_encrypt(&enc.key, iv, data);
            iv.to_vec()
        }
        Scheme::CbcsPattern => {
            let iv = cbc_iv(&enc.key, counter);
            apply_cbc_pattern(&enc.key, iv, data, PATTERN_CRYPT, PATTERN_SKIP);
            iv.to_vec()
        }
    }
}

/// Rewrites a sample entry (`vp09`, `Opus`, ...) as its protected form (`encv`
/// or `enca`): the same body, plus a `sinf` box that names the original format
/// and carries the `tenc` defaults (protected, the scheme's IV size, the key ID).
pub fn protect_entry(entry: &[u8], audio: bool, enc: &Encryption) -> Result<Vec<u8>, MuxError> {
    if entry.len() < 8 {
        return Err(MuxError::InvalidConfig("sample entry is truncated".into()));
    }
    let original: [u8; 4] = [entry[4], entry[5], entry[6], entry[7]];
    // tenc: version 0 (CENC, no pattern) or version 1 (CBCS, pattern byte 0x00 =
    // no pattern). Both name the protected flag, the per-sample IV size and the KID.
    let tenc_body = match enc.scheme {
        Scheme::Cenc => {
            let mut b = vec![0u8, 0, 1, enc.scheme.iv_len() as u8];
            b.extend_from_slice(&enc.key_id);
            full_box(b"tenc", 0, 0, &b)
        }
        Scheme::Cbcs => {
            let mut b = vec![0u8, 0, 1, enc.scheme.iv_len() as u8];
            b.extend_from_slice(&enc.key_id);
            full_box(b"tenc", 1, 0, &b)
        }
        Scheme::CbcsPattern => {
            // The second byte is default_crypt_byte_block << 4 | default_skip_byte_block.
            let pattern = ((PATTERN_CRYPT as u8) << 4) | PATTERN_SKIP as u8;
            let mut b = vec![0u8, pattern, 1, enc.scheme.iv_len() as u8];
            b.extend_from_slice(&enc.key_id);
            full_box(b"tenc", 1, 0, &b)
        }
    };
    let mut schm_body = enc.scheme.fourcc().to_vec();
    schm_body.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    let sinf = boxed(
        b"sinf",
        &[
            boxed(b"frma", &original),
            full_box(b"schm", 0, 0, &schm_body),
            boxed(b"schi", &tenc_body),
        ]
        .concat(),
    );
    let kind: &[u8; 4] = if audio { b"enca" } else { b"encv" };
    let mut body = entry[8..].to_vec();
    body.extend_from_slice(&sinf);
    Ok(boxed(kind, &body))
}

/// The ClearKey `pssh` box (system ID `1077efec-c0b2-4d02-ace3-3c1e52e2fb4b`,
/// version 1) naming one key ID. Players fire their encrypted event for it: an
/// HLS player (hls.js) only creates a key session when it sees a `pssh`.
pub fn pssh_clearkey(key_id: &[u8; 16]) -> Vec<u8> {
    let mut body = vec![
        0x10, 0x77, 0xef, 0xec, 0xc0, 0xb2, 0x4d, 0x02, 0xac, 0xe3, 0x3c, 0x1e, 0x52, 0xe2, 0xfb,
        0x4b,
    ];
    body.extend_from_slice(&1u32.to_be_bytes()); // KID_count
    body.extend_from_slice(key_id);
    body.extend_from_slice(&0u32.to_be_bytes()); // Data_size
    full_box(b"pssh", 1, 0, &body)
}

/// The `senc` box for one fragment's samples: one IV per sample (each of the
/// scheme's IV length), no subsample map.
/// `senc` with one subsample per sample, the whole sample protected (clear 0
/// bytes, encrypted `size`). A pattern scheme is applied within the protected
/// bytes of each subsample, so this is what makes the 1:9 pattern apply to a
/// whole VP9 or AV1 sample. ffmpeg decrypts that form, and ignores the pattern
/// for a sample with no subsample map.
pub fn senc_with_subsamples(ivs: &[Vec<u8>], sizes: &[usize]) -> Vec<u8> {
    let mut body = (ivs.len() as u32).to_be_bytes().to_vec();
    for (iv, size) in ivs.iter().zip(sizes) {
        body.extend_from_slice(iv);
        body.extend_from_slice(&1u16.to_be_bytes()); // subsample_count
        body.extend_from_slice(&0u16.to_be_bytes()); // bytes_of_clear_data
        body.extend_from_slice(&(*size as u32).to_be_bytes()); // bytes_of_protected_data
    }
    full_box(b"senc", 0, 0x2, &body)
}

pub fn senc(ivs: &[Vec<u8>]) -> Vec<u8> {
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
    fn ctr_matches_nist_sp800_38a_vector() {
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
    fn cbc_matches_nist_sp800_38a_vector() {
        // NIST SP 800-38A, F.2.1 (CBC-AES128.Encrypt), first two blocks.
        let key = parse_hex16("2b7e151628aed2a6abf7158809cf4f3c").unwrap();
        let iv = parse_hex16("000102030405060708090a0b0c0d0e0f").unwrap();
        let mut data = [
            parse_hex16("6bc1bee22e409f96e93d7e117393172a").unwrap(),
            parse_hex16("ae2d8a571e03ac9c9eb76fac45af8e51").unwrap(),
        ]
        .concat();
        apply_cbc_encrypt(&key, iv, &mut data);
        assert_eq!(
            hex(&data),
            "7649abac8119b246cee98e9b12e9197d5086cb9b507219ee95db113a917678b2"
        );
    }

    #[test]
    fn cbc_leaves_a_partial_block_clear() {
        let key = [3u8; 16];
        let mut data = vec![0x55u8; 20];
        apply_cbc_encrypt(&key, [0u8; 16], &mut data);
        assert_ne!(&data[..16], &[0x55u8; 16][..]);
        assert_eq!(&data[16..], &[0x55u8; 4][..]);
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
    fn cbcs_ivs_are_unique_per_counter() {
        let key = [9u8; 16];
        assert_ne!(cbc_iv(&key, 0), cbc_iv(&key, 1));
        assert_eq!(cbc_iv(&key, 5), cbc_iv(&key, 5));
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
    fn scheme_names_parse() {
        assert_eq!(Scheme::parse("CENC"), Some(Scheme::Cenc));
        assert_eq!(Scheme::parse("cbcs"), Some(Scheme::Cbcs));
        assert_eq!(Scheme::parse("cbc1"), None);
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
            scheme: Scheme::Cbcs,
        };
        let w = FragmentWriter::new(&[s]).unwrap();
        assert!(w.with_encryption(Some(enc)).is_err());
    }

    #[test]
    fn clearkey_pssh_names_the_key_id() {
        let kid = [0x42u8; 16];
        let b = pssh_clearkey(&kid);
        assert_eq!(&b[4..8], b"pssh");
        assert_eq!(b[8], 1, "version 1 lists its key IDs");
        assert_eq!(
            &b[12..28],
            &[
                0x10, 0x77, 0xef, 0xec, 0xc0, 0xb2, 0x4d, 0x02, 0xac, 0xe3, 0x3c, 0x1e, 0x52, 0xe2,
                0xfb, 0x4b
            ]
        );
        assert_eq!(&b[28..32], &1u32.to_be_bytes(), "one key ID");
        assert_eq!(&b[32..48], &kid);
        assert_eq!(&b[48..52], &0u32.to_be_bytes(), "no system data");
        assert_eq!(b.len(), 52);
    }

    #[test]
    fn debug_never_prints_the_key() {
        let e = Encryption {
            key_id: [1; 16],
            key: [0xAB; 16],
            scheme: Scheme::Cenc,
        };
        let shown = format!("{e:?}");
        assert!(!shown.contains("ab"), "{shown}");
        assert!(shown.contains("redacted"));
    }

    #[test]
    fn cbcs_pattern_encrypts_one_block_in_ten_and_chains_over_them() {
        let key = [0x11u8; 16];
        let iv = [0x22u8; 16];
        let clear: Vec<u8> = (0..16 * 12).map(|i| i as u8).collect();
        let mut got = clear.clone();
        apply_cbc_pattern(&key, iv, &mut got, 1, 9);
        // Block 0 is CBC-encrypted from the IV; blocks 1 to 9 stay clear.
        let mut b0 = clear[..16].to_vec();
        apply_cbc_encrypt(&key, iv, &mut b0);
        assert_eq!(&got[..16], &b0[..]);
        assert_eq!(&got[16..160], &clear[16..160]);
        // Block 10 is encrypted, chained from block 0's ciphertext: a skipped block
        // does not feed the chain.
        let mut b10 = clear[160..176].to_vec();
        apply_cbc_encrypt(&key, b0.as_slice().try_into().unwrap(), &mut b10);
        assert_eq!(&got[160..176], &b10[..]);
        // Block 11 is clear.
        assert_eq!(&got[176..], &clear[176..]);
    }
}
