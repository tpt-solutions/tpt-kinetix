//! Debug helper: decode an Annex B .h264 file and dump first sample values.
//! Usage: cargo run -p tpt-kinetix-h264 --example dbg_decode -- <file.h264> <refyuv?>
use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::H264Decoder;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = &args[1];
    let data = std::fs::read(path).unwrap();
    let mut dec = H264Decoder::new();
    let pkt = Packet {
        pts: Timestamp::new(0, (1, 30)),
        dts: Timestamp::new(0, (1, 30)),
        data,
        stream_index: 0,
        is_key_frame: true,
    };
    match dec.decode(&pkt) {
        Ok(Some(f)) => {
            println!("decoded {}x{} data_len={}", f.width, f.height, f.data.len());
            let y = &f.data;
            println!("Y[0..16]: {:?}", &y[0..16.min(y.len())]);
            let luma = (f.width * f.height) as usize;
            println!("Cb[0..8]: {:?}", &y[luma..(luma + 8).min(y.len())]);
            if args.len() > 2 {
                let refy = std::fs::read(&args[2]).unwrap();
                let n = refy.len().min(y.len());
                let mut maxd = 0i32;
                let mut nd = 0;
                for i in 0..n {
                    let d = (refy[i] as i32 - y[i] as i32).abs();
                    if d != 0 { nd += 1; maxd = maxd.max(d); }
                }
                println!("ref Y[0..16]: {:?}", &refy[0..16.min(refy.len())]);
                println!("vs ref: max_diff={maxd} ndiff={nd}/{n}");
                let _ = std::fs::write("C:\\Users\\phill\\AppData\\Local\\Temp\\kilo\\h264flat\\out.yuv", y);
            }
        }
        Ok(None) => println!("no frame produced"),
        Err(e) => println!("error: {e}"),
    }
}
