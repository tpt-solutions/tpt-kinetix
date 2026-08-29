/* Minimal stubs so the vendored ffmpeg CABAC engine + decode can compile
 * standalone for the oracle. No libav linking required. */
#ifndef AVCODEC_CABAC_ORACLE_STUBS_H
#define AVCODEC_CABAC_ORACLE_STUBS_H

#include <stdint.h>
#include <stddef.h>
#include <string.h>
#include <stdio.h>
#include <stdlib.h>

#define av_always_inline inline
#define av_unused
#define av_noinline
#define av_assert0(x)
#define av_assert2(x)
#define AVERROR_INVALIDDATA -1
#define AV_PICTURE_TYPE_P 0
#define AV_LOG_ERROR 0
static inline void av_log(void *a, int b, const char *fmt, (void)) { (void)a;(void)b;(void)fmt; }
#define ff_tlog(a, fmt, ...) ((void)0)

/* H.264 profile/enum stubs used by the reference */
#define MB_TYPE_16x16      0x0001
#define MB_TYPE_16x8       0x0002
#define MB_TYPE_8x16       0x0004
#define MB_TYPE_8x8        0x0008
#define MB_TYPE_INTRA      0x0080
#define MB_TYPE_INTERLACED 0x0800
#define MB_TYPE_8x8DCT     0x01000000
#define IS_INTRA(a)        ((a) & MB_TYPE_INTRA)
#define IS_INTRA4x4(a)     0
#define IS_8x8DCT(a)       ((a) & MB_TYPE_8x8DCT)
#define IS_DIRECT(a)       0
#define IS_DIR(a,b,c)      1
#define FRAME_MBAFF(h)     1
#define MB_FIELD(sl)       ((sl)->mb_field_decoding_flag)
#define MB_MBAFF(sl)       ((sl)->mb_field_decoding_flag)
#define PART_NOT_AVAILABLE 0

#endif
