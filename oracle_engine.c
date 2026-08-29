/* oracle_engine.c — ffmpeg CABAC arithmetic engine (tables from oracle_tables.c). */
#include "oracle_engine.h"

static const uint8_t * const ff_h264_norm_shift = ff_h264_cabac_tables + 0;
static const uint8_t * const ff_h264_lps_range  = ff_h264_cabac_tables + 512;
static const uint8_t * const ff_h264_mlps_state = ff_h264_cabac_tables + 1024;

int ff_init_cabac_decoder(CABACContext *c, const uint8_t *buf, int buf_size){
    c->bytestream_start = c->bytestream = buf;
    c->bytestream_end = buf + buf_size;
    c->low = (*c->bytestream++) << 18;
    c->low += (*c->bytestream++) << 10;
    if(((uintptr_t)c->bytestream & 1) == 0) c->low += (1 << 9);
    else c->low += ((*c->bytestream++) << 2) + 2;
    c->range = 0x1FE;
    if ((c->range << (CABAC_BITS+1)) < c->low) return -1;
    return 0;
}

static inline void refill(CABACContext *c){
    c->low += (c->bytestream[0]<<9) + (c->bytestream[1]<<1);
    c->low -= CABAC_MASK;
    c->bytestream += CABAC_BITS/8;
}
static inline void refill2(CABACContext *c){
    int i; unsigned x;
    i = __builtin_ctz((unsigned)c->low) - CABAC_BITS;
    x = -CABAC_MASK;
    x += (c->bytestream[0]<<9) + (c->bytestream[1]<<1);
    c->low += (int)(x<<i);
    c->bytestream += CABAC_BITS/8;
}
static inline void renorm_cabac_decoder_once(CABACContext *c){
    int shift = (unsigned)(c->range - 0x100)>>31;
    c->range <<= shift; c->low <<= shift;
    if(!(c->low & CABAC_MASK)) refill(c);
}
int get_cabac_terminate(CABACContext *c){
    c->range -= 2;
    if(c->low < c->range<<(CABAC_BITS+1)){ renorm_cabac_decoder_once(c); return 0; }
    else return c->bytestream - c->bytestream_start;
}
int get_cabac_bypass(CABACContext *c){
    int range;
    c->low += c->low;
    if(!(c->low & CABAC_MASK)) refill(c);
    range = c->range << (CABAC_BITS+1);
    if(c->low < range) return 0;
    else { c->low -= range; return 1; }
}
int get_cabac(CABACContext *c, uint8_t * const state){
    int s = *state;
    int RangeLPS = ff_h264_lps_range[2*(c->range&0xC0) + s];
    int bit, lps_mask;
    c->range -= RangeLPS;
    lps_mask = ((c->range<<(CABAC_BITS+1)) - c->low)>>31;
    c->low -= (c->range<<(CABAC_BITS+1)) & lps_mask;
    c->range += (RangeLPS - c->range) & lps_mask;
    s ^= lps_mask;
    *state = (ff_h264_mlps_state+128)[s];
    bit = s & 1;
    lps_mask = ff_h264_norm_shift[c->range];
    c->range <<= lps_mask; c->low <<= lps_mask;
    if(!(c->low & CABAC_MASK)) refill2(c);
    return bit;
}
