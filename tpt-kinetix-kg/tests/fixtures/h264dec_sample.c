/*
 * FFmpeg-style H.264 decoder source excerpt (synthetic fixture).
 *
 * This file is a hand-written, self-contained approximation of the shapes found
 * in FFmpeg's libavcodec/h264dec.c and friends.  It deliberately mirrors the
 * structural patterns the kinetix-kg extraction passes look for:
 *   - NAL-unit-type dispatch via a `switch` (bitstream parsing states)
 *   - a macroblock processing `enum` (state machine)
 *   - per-slice / per-macroblock loops (parallelism candidates)
 *   - a call graph between decode functions
 *
 * It is NOT a real, buildable decoder — it exists only so Phase 1 tooling can
 * be validated end-to-end without vendoring FFmpeg's GPL/LGPL source.
 */

enum MacroblockType {
    MB_TYPE_INTRA_4x4,
    MB_TYPE_INTRA_16x16,
    MB_TYPE_INTRA_PCM,
    MB_TYPE_INTER_16x16,
    MB_TYPE_INTER_8x8,
    MB_TYPE_SKIP,
};

enum NalUnitType {
    NAL_SLICE = 1,
    NAL_DPA = 2,
    NAL_IDR_SLICE = 5,
    NAL_SEI = 6,
    NAL_SPS = 7,
    NAL_PPS = 8,
};

static int decode_sps(void *ctx, const unsigned char *buf, int size) {
    return 0;
}

static int decode_pps(void *ctx, const unsigned char *buf, int size) {
    return 0;
}

static int decode_slice_header(void *ctx, const unsigned char *buf, int size) {
    return 0;
}

static int decode_macroblock(void *ctx, int mb_x, int mb_y) {
    return 0;
}

static int decode_slice_data(void *ctx, int first_mb, int mb_count) {
    int mb;
    /* Per-macroblock loop: candidate for data parallelism. */
    for (mb = first_mb; mb < first_mb + mb_count; mb++) {
        decode_macroblock(ctx, mb % 16, mb / 16);
    }
    return 0;
}

static int decode_slice(void *ctx, const unsigned char *buf, int size) {
    decode_slice_header(ctx, buf, size);
    decode_slice_data(ctx, 0, 256);
    return 0;
}

int h264_decode_nal(void *ctx, const unsigned char *buf, int size) {
    int nal_type = buf[0] & 0x1f;

    /* NAL-unit-type dispatch: the primary bitstream parsing state machine. */
    switch (nal_type) {
    case NAL_SPS:
        return decode_sps(ctx, buf, size);
    case NAL_PPS:
        return decode_pps(ctx, buf, size);
    case NAL_IDR_SLICE:
    case NAL_SLICE:
        return decode_slice(ctx, buf, size);
    case NAL_SEI:
        return 0;
    default:
        return -1;
    }
}

int h264_decode_frame(void *ctx, const unsigned char *buf, int size) {
    int offset = 0;
    /* Iterate over NAL units in the access unit. */
    while (offset < size) {
        int nal_size = buf[offset];
        h264_decode_nal(ctx, buf + offset + 1, nal_size);
        offset += nal_size + 1;
    }
    return 0;
}
