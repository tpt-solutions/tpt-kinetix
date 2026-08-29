/* Minimal stubs so ffmpeg's cabac_ref.c compiles standalone. */
#ifndef AVCODEC_CABAC_H
#define AVCODEC_CABAC_H
#include <stdint.h>
extern const uint8_t ff_h264_cabac_tables[512 + 4*2*64 + 4*63];
#define H264_NORM_SHIFT_OFFSET 0
#define H264_LPS_RANGE_OFFSET 512
#define H264_MLPS_STATE_OFFSET 1024
#define H264_LAST_COEFF_FLAG_OFFSET_8X8_OFFSET 1280
#define CABAC_BITS 16
#define CABAC_MASK ((1<<CABAC_BITS)-1)
typedef struct CABACContext{
    int low;
    int range;
    const uint8_t *bytestream_start;
    const uint8_t *bytestream;
    const uint8_t *bytestream_end;
}CABACContext;
int ff_init_cabac_decoder(CABACContext *c, const uint8_t *buf, int buf_size);
#endif

#ifndef AVCODEC_CABAC_FUNCTIONS_H
#define AVCODEC_CABAC_FUNCTIONS_H
#include "cabac.h"
#include <stddef.h>
#define av_always_inline inline
#define av_unused
#define UNCHECKED_BITSTREAM_READER 1
int get_cabac_noinline(CABACContext *c, uint8_t * const state);
int get_cabac(CABACContext *c, uint8_t * const state);
int get_cabac_bypass(CABACContext *c);
int get_cabac_terminate(CABACContext *c);
#endif

/* avutil stubs */
#ifndef AVUTIL_ATTRIBUTES_H
#define AVUTIL_ATTRIBUTES_H
#endif
#ifndef AVUTIL_INT_MATH_H
#define AVUTIL_INT_MATH_H
#include <stdint.h>
#endif
#ifndef CONFIG_H
#define CONFIG_H
#define CONFIG_SAFE_BITSTREAM_READER 0
#define HAVE_FAST_CLZ 1
#define ARCH_X86 0
#define ARCH_AARCH64 0
#define ARCH_ARM 0
#define ARCH_MIPS 0
#define ARCH_LOONGARCH64 0
#endif
