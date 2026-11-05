//! A minimal PNG encoder for 8-bit RGB thumbnails.
//!
//! The image data is wrapped in zlib *stored* blocks (no compression), so the
//! encoder needs no dependency. A thumbnail is a few hundred kilobytes that way,
//! which is an acceptable trade for a tool that writes a handful of stills.

/// The 8-byte PNG file signature.
const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
/// The largest payload of one stored deflate block.
const STORED_BLOCK_MAX: usize = 65_535;

/// Encodes `rgb` (`width * height * 3` bytes, row-major, R G B) as a PNG file.
pub fn encode_rgb(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
    let row = width as usize * 3;
    debug_assert_eq!(rgb.len(), row * height as usize);

    // Scanlines, each prefixed with filter type 0 (none).
    let mut raw = Vec::with_capacity((row + 1) * height as usize);
    for line in rgb.chunks(row.max(1)) {
        raw.push(0);
        raw.extend_from_slice(line);
    }

    let mut out = SIGNATURE.to_vec();
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    // Bit depth 8, colour type 2 (truecolour), deflate, no filter variants, no interlace.
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    push_chunk(&mut out, b"IHDR", &ihdr);
    push_chunk(&mut out, b"IDAT", &zlib_stored(&raw));
    push_chunk(&mut out, b"IEND", &[]);
    out
}

fn push_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut covered = Vec::with_capacity(4 + data.len());
    covered.extend_from_slice(kind);
    covered.extend_from_slice(data);
    out.extend_from_slice(&crc32(&covered).to_be_bytes());
}

/// A zlib stream holding `data` in stored (uncompressed) deflate blocks.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01]; // CMF/FLG: deflate, 32 KiB window, no dictionary
    let mut chunks = data.chunks(STORED_BLOCK_MAX).peekable();
    if chunks.peek().is_none() {
        // An empty payload still needs one final block.
        out.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
    }
    while let Some(block) = chunks.next() {
        let last = chunks.peek().is_none();
        let len = block.len() as u16;
        out.push(u8::from(last)); // BFINAL, BTYPE = 00 (stored)
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(block);
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn adler32(bytes: &[u8]) -> u32 {
    const MOD: u32 = 65_521;
    let (mut a, mut b) = (1u32, 0u32);
    for &x in bytes {
        a = (a + u32::from(x)) % MOD;
        b = (b + a) % MOD;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums_match_their_reference_values() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }

    #[test]
    fn encodes_a_two_by_one_image() {
        let rgb = [255, 0, 0, 0, 255, 0];
        let png = encode_rgb(2, 1, &rgb);
        assert_eq!(&png[..8], &SIGNATURE);
        // IHDR follows the signature: length 13, then the dimensions.
        assert_eq!(&png[8..12], &13u32.to_be_bytes());
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(&png[16..20], &2u32.to_be_bytes());
        assert_eq!(&png[20..24], &1u32.to_be_bytes());
        assert!(png.ends_with(&[0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82]));
    }

    #[test]
    fn large_images_split_into_several_stored_blocks() {
        // 300 x 100 x 3 bytes of scanlines is over one 64 KiB block.
        let rgb = vec![7u8; 300 * 100 * 3];
        let png = encode_rgb(300, 100, &rgb);
        assert_eq!(&png[..8], &SIGNATURE);
        assert!(png.len() > rgb.len());
    }
}
