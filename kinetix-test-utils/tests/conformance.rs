// Tests that run across codec/demux boundaries using kinetix-test-utils helpers.
use kinetix_test_utils::pixel_diff::*;
use kinetix_test_utils::synthetic::*;

#[test]
fn grey_frame_is_identical_to_itself() {
    let frame = grey_yuv420p_frame(64, 64);
    assert!(within_tolerance(&frame, &frame, 0));
    let (y, cb, cr) = psnr_yuv420p(&frame, &frame).unwrap();
    assert!(y.is_infinite() && cb.is_infinite() && cr.is_infinite());
}

#[test]
fn ramp_frame_differs_from_grey() {
    let grey = grey_yuv420p_frame(64, 64);
    let ramp = ramp_yuv420p_frame(64, 64);
    assert!(!within_tolerance(&grey, &ramp, 0));
    let count = luma_diff_count(&grey, &ramp);
    assert!(count > 0);
}

#[test]
fn corpus_edge_cases_do_not_panic() {
    use kinetix_test_utils::corpus::Corpus;
    let mut c = Corpus::new("demux");
    c.add_edge_cases();
    for entry in c.iter() {
        let _ = kinetix_demux::mp4::container::parse_mp4(&entry.data);
    }
}
