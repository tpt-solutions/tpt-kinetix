use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::trace::{DecodeTracer, TracePlane};
use tpt_kinetix_h264::H264Decoder;

#[derive(Default)]
struct DumpTracer {
    dc: Vec<(u32, u32, [i16; 16])>,
    mbs: Vec<(u32, u32, String, i32, u8, u8, [u8; 16])>,
}

impl DecodeTracer for DumpTracer {
    fn on_cavlc_coeffs(&mut self, mb_x: u32, mb_y: u32, plane: TracePlane, blk: u8, coeffs: &[i16; 16]) {
        if plane == TracePlane::Luma && blk == 16 {
            self.dc.push((mb_x, mb_y, *coeffs));
        }
    }
    fn on_mb_parsed(
        &mut self,
        mb_x: u32,
        mb_y: u32,
        mb_type: &str,
        qp: i32,
        cbp: u8,
        chroma_mode: u8,
        modes: &[u8; 16],
    ) {
        self.mbs.push((mb_x, mb_y, mb_type.to_string(), qp, cbp, chroma_mode, *modes));
    }
}

#[test]
fn diag() {
    let dir = std::env::temp_dir().join("tpt_kinetix_h264_conformance");
    let annexb = std::fs::read(dir.join("t_nodblk.h264")).unwrap();
    let refbytes = std::fs::read(dir.join("t_nodblk.yuv")).unwrap();

    let mut dec = H264Decoder::new();
    let mut tr = DumpTracer::default();
    let pkt = Packet {
        pts: Timestamp::new(0, (1, 30)),
        dts: Timestamp::new(0, (1, 30)),
        data: annexb,
        stream_index: 0,
        is_key_frame: true,
    };
    let frame = dec.decode_with_tracer(&pkt, &mut tr).unwrap().unwrap();
    let w = 64usize;
    let luma = &frame.data[..w * 48];

    for (x, y, mt, qp, cbp, chm, _modes) in &tr.mbs {
        println!("MB({x},{y}) {mt} qp={qp} cbp={cbp} chroma_mode={chm}");
    }
    println!();
    for (x, y, dc) in &tr.dc {
        println!("MB({x},{y}) luma_dc(zigzag): {dc:?}");
    }
    println!();
    // Per-MB mean diff (ours - ref) for luma, 16x16 block grid.
    for y in 0..3usize {
        for x in 0..4usize {
            let mut sum = 0i64;
            for r in y * 16..y * 16 + 16 {
                for c in x * 16..x * 16 + 16 {
                    sum += luma[r * w + c] as i64 - refbytes[r * w + c] as i64;
                }
            }
            print!("d({x},{y})={:+5.1}  ", sum as f64 / 256.0);
        }
        println!();
    }
}
