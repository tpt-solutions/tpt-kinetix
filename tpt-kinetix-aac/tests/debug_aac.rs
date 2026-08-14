//! Throwaway debug harness to locate the AAC parse desync.

#[test]
fn debug_first_frame() {
    let adts = match tpt_kinetix_test_utils::synthetic::minimal_aac_adts(44_100, 2, 0.25) {
        Some(a) => a,
        None => {
            eprintln!("skip: no stream");
            return;
        }
    };
    eprintln!("stream len = {}", adts.len());

    let hdr = tpt_kinetix_aac::adts::AdtsHeader::parse(&adts).unwrap();
    eprintln!(
        "hdr: sf_idx={} ch={} frame_len={} header_len={}",
        hdr.sampling_frequency_index, hdr.channels, hdr.frame_length, hdr.header_len
    );
    let payload = &adts[hdr.header_len..hdr.frame_length];
    eprintln!("payload len = {}", payload.len());

    // Dump first 48 bytes as bits.
    for (i, &b) in payload.iter().take(48).enumerate() {
        eprintln!(
            "byte[{i:2}] {b:08b}  0x{b:02x}"
        );
    }
    // Dump a bit string for the first 160 bits.
    let mut s = String::new();
    for bitpos in 0..160u32 {
        let byte = (bitpos / 8) as usize;
        let bit = (payload[byte] >> (7 - (bitpos % 8))) & 1;
        s.push(if bit == 1 { '1' } else { '0' });
        if (bitpos + 1) % 8 == 0 {
            s.push(' ');
        }
    }
    eprintln!("bits0..160: {s}");

    // Try parse and report.
    match tpt_kinetix_aac::syntax::RawDataBlock::parse(payload, hdr.sampling_frequency_index as usize) {
        Ok(block) => eprintln!("parsed OK, {} elements", block.elements.len()),
        Err(e) => eprintln!("parse error: {e:?}"),
    }
}
