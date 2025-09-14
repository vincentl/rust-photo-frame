use rust_photo_frame::tasks::viewer::compute_padded_stride;

#[test]
fn stride_alignment_rounds_up_to_256() {
    assert_eq!(compute_padded_stride(0), 0);
    assert_eq!(compute_padded_stride(4), 256);
    assert_eq!(compute_padded_stride(255), 256);
    assert_eq!(compute_padded_stride(256), 256);
    assert_eq!(compute_padded_stride(257), 512);
    assert_eq!(compute_padded_stride(1024), 1024);
    assert_eq!(compute_padded_stride(1028), 1280);
}

