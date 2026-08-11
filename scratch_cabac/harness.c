/* Self-contained harness using FFmpeg's REAL cabac.c engine (copied verbatim,
 * with av_* macros stripped to plain C) to independently decode the same
 * CABAC payload the Rust/Python implementations processed. */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define CABAC_BITS 16
#define CABAC_MASK ((1<<CABAC_BITS)-1)
#define UNCHECKED_BITSTREAM_READER 1

typedef struct CABACContext {
    int low;
    int range;
    const uint8_t *bytestream_start;
    const uint8_t *bytestream;
    const uint8_t *bytestream_end;
} CABACContext;

/* ff_h264_cabac_tables, copied verbatim from FFmpeg's cabac.c */
static const uint8_t ff_h264_cabac_tables[512 + 4*2*64 + 4*64 + 63] = {
    9,8,7,7,6,6,6,6,5,5,5,5,5,5,5,5,
    4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,
    3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,
    3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,
    2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,
    2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,
    2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,
    2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,
    1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,
    1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,
    1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,
    1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
    (uint8_t)-128,(uint8_t)-128,(uint8_t)-128,(uint8_t)-128,(uint8_t)-128,(uint8_t)-128,123,123,
    116,116,111,111,105,105,100,100,
    95,95,90,90,85,85,81,81,
    77,77,73,73,69,69,66,66,
    62,62,59,59,56,56,53,53,
    51,51,48,48,46,46,43,43,
    41,41,39,39,37,37,35,35,
    33,33,32,32,30,30,29,29,
    27,27,26,26,24,24,23,23,
    22,22,21,21,20,20,19,19,
    18,18,17,17,16,16,15,15,
    14,14,14,14,13,13,12,12,
    12,12,11,11,11,11,10,10,
    10,10,9,9,9,9,8,8,
    8,8,7,7,7,7,7,7,
    6,6,6,6,6,6,2,2,
    (uint8_t)-80,(uint8_t)-80,(uint8_t)-89,(uint8_t)-89,(uint8_t)-98,(uint8_t)-98,(uint8_t)-106,(uint8_t)-106,
    (uint8_t)-114,(uint8_t)-114,(uint8_t)-121,(uint8_t)-121,(uint8_t)-128,(uint8_t)-128,122,122,
    116,116,110,110,104,104,99,99,
    94,94,89,89,85,85,80,80,
    76,76,72,72,69,69,65,65,
    62,62,59,59,56,56,53,53,
    50,50,48,48,45,45,43,43,
    41,41,39,39,37,37,35,35,
    33,33,31,31,30,30,28,28,
    27,27,26,26,24,24,23,23,
    22,22,21,21,20,20,19,19,
    18,18,17,17,16,16,15,15,
    14,14,14,14,13,13,12,12,
    12,12,11,11,11,11,10,10,
    9,9,9,9,9,9,8,8,
    8,8,7,7,7,7,2,2,
    (uint8_t)-48,(uint8_t)-48,(uint8_t)-59,(uint8_t)-59,(uint8_t)-69,(uint8_t)-69,(uint8_t)-78,(uint8_t)-78,
    (uint8_t)-87,(uint8_t)-87,(uint8_t)-96,(uint8_t)-96,(uint8_t)-104,(uint8_t)-104,(uint8_t)-112,(uint8_t)-112,
    (uint8_t)-119,(uint8_t)-119,(uint8_t)-126,(uint8_t)-126,123,123,117,117,
    111,111,105,105,100,100,95,95,
    90,90,86,86,81,81,77,77,
    73,73,69,69,66,66,63,63,
    59,59,56,56,54,54,51,51,
    48,48,46,46,43,43,41,41,
    39,39,37,37,35,35,33,33,
    32,32,30,30,29,29,27,27,
    26,26,25,25,23,23,22,22,
    21,21,20,20,19,19,18,18,
    17,17,16,16,15,15,15,15,
    14,14,13,13,12,12,12,12,
    11,11,11,11,10,10,10,10,
    9,9,9,9,8,8,2,2,
    (uint8_t)-16,(uint8_t)-16,(uint8_t)-29,(uint8_t)-29,(uint8_t)-40,(uint8_t)-40,(uint8_t)-51,(uint8_t)-51,
    (uint8_t)-61,(uint8_t)-61,(uint8_t)-71,(uint8_t)-71,(uint8_t)-81,(uint8_t)-81,(uint8_t)-90,(uint8_t)-90,
    (uint8_t)-98,(uint8_t)-98,(uint8_t)-106,(uint8_t)-106,(uint8_t)-114,(uint8_t)-114,(uint8_t)-121,(uint8_t)-121,
    (uint8_t)-128,(uint8_t)-128,122,122,116,116,110,110,
    104,104,99,99,94,94,89,89,
    85,85,80,80,76,76,72,72,
    69,69,65,65,62,62,59,59,
    56,56,53,53,50,50,48,48,
    45,45,43,43,41,41,39,39,
    37,37,35,35,33,33,31,31,
    30,30,28,28,27,27,25,25,
    24,24,23,23,22,22,21,21,
    20,20,19,19,18,18,17,17,
    16,16,15,15,14,14,14,14,
    13,13,12,12,12,12,11,11,
    11,11,10,10,9,9,2,2,
    /* mlps state */
    127,126,77,76,77,76,75,74,
    75,74,75,74,73,72,73,72,
    73,72,71,70,71,70,71,70,
    69,68,69,68,67,66,67,66,
    67,66,65,64,65,64,63,62,
    61,60,61,60,61,60,59,58,
    59,58,57,56,55,54,55,54,
    53,52,53,52,51,50,49,48,
    49,48,47,46,45,44,45,44,
    43,42,43,42,39,38,39,38,
    37,36,37,36,33,32,33,32,
    31,30,31,30,27,26,27,26,
    25,24,23,22,23,22,19,18,
    19,18,17,16,15,14,13,12,
    11,10,9,8,9,8,5,4,
    5,4,3,2,1,0,0,1,
    2,3,4,5,6,7,8,9,
    10,11,12,13,14,15,16,17,
    18,19,20,21,22,23,24,25,
    26,27,28,29,30,31,32,33,
    34,35,36,37,38,39,40,41,
    42,43,44,45,46,47,48,49,
    50,51,52,53,54,55,56,57,
    58,59,60,61,62,63,64,65,
    66,67,68,69,70,71,72,73,
    74,75,76,77,78,79,80,81,
    82,83,84,85,86,87,88,89,
    90,91,92,93,94,95,96,97,
    98,99,100,101,102,103,104,105,
    106,107,108,109,110,111,112,113,
    114,115,116,117,118,119,120,121,
    122,123,124,125,124,125,126,127,
    /* last_coeff_flag_offset_8x8 */
    0,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,
    2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,
    3,3,3,3,3,3,3,3,4,4,4,4,4,4,4,4,
    5,5,5,5,6,6,6,6,7,7,7,7,8,8,8
};

#define H264_NORM_SHIFT_OFFSET 0
#define H264_LPS_RANGE_OFFSET 512
#define H264_MLPS_STATE_OFFSET 1024

static const uint8_t * const ff_h264_norm_shift = ff_h264_cabac_tables + H264_NORM_SHIFT_OFFSET;
static const uint8_t * const ff_h264_lps_range = ff_h264_cabac_tables + H264_LPS_RANGE_OFFSET;
static const uint8_t * const ff_h264_mlps_state = ff_h264_cabac_tables + H264_MLPS_STATE_OFFSET;

int ff_init_cabac_decoder(CABACContext *c, const uint8_t *buf, int buf_size){
    c->bytestream_start = c->bytestream = buf;
    c->bytestream_end = buf + buf_size;
    c->low = (*c->bytestream++)<<18;
    c->low += (*c->bytestream++)<<10;
    if (((uintptr_t)c->bytestream & 1) == 0) {
        c->low += (1 << 9);
    } else {
        c->low += ((*c->bytestream++) << 2) + 2;
    }
    c->range = 0x1FE;
    return 0;
}

static void refill(CABACContext *c){
    c->low += (c->bytestream[0]<<9) + (c->bytestream[1]<<1);
    c->low -= CABAC_MASK;
    c->bytestream += CABAC_BITS / 8;
}

static void refill2(CABACContext *c){
    int i;
    unsigned x;
    x = c->low ^ (c->low-1);
    i = 7 - ff_h264_norm_shift[x>>(CABAC_BITS-1)];
    x = -CABAC_MASK;
    x += (c->bytestream[0]<<9) + (c->bytestream[1]<<1);
    c->low += x<<i;
    c->bytestream += CABAC_BITS/8;
}

static inline int get_cabac_inline(CABACContext *c, uint8_t * const state){
    int s = *state;
    int RangeLPS = ff_h264_lps_range[2*(c->range&0xC0) + s];
    int bit, lps_mask;

    c->range -= RangeLPS;
    lps_mask = ((c->range<<(CABAC_BITS+1)) - c->low)>>31;

    c->low -= (c->range<<(CABAC_BITS+1)) & lps_mask;
    c->range += (RangeLPS - c->range) & lps_mask;

    s ^= lps_mask;
    *state = (ff_h264_mlps_state+128)[s];
    bit = s&1;

    lps_mask = ff_h264_norm_shift[c->range];
    c->range <<= lps_mask;
    c->low <<= lps_mask;
    if (!(c->low & CABAC_MASK))
        refill2(c);
    return bit;
}

static int get_cabac(CABACContext *c, uint8_t * const state){
    return get_cabac_inline(c, state);
}

static int get_cabac_bypass(CABACContext *c){
    int range;
    c->low += c->low;
    if (!(c->low & CABAC_MASK))
        refill(c);
    range = c->range<<(CABAC_BITS+1);
    if (c->low < range) {
        return 0;
    } else {
        c->low -= range;
        return 1;
    }
}

static inline int get_cabac_bypass_sign(CABACContext *c, int val){
    int range, mask;
    c->low += c->low;
    if (!(c->low & CABAC_MASK))
        refill(c);
    range = c->range<<(CABAC_BITS+1);
    c->low -= range;
    mask = c->low >> 31;
    range &= mask;
    c->low += range;
    return (val^mask)-mask;
}

static void renorm_cabac_decoder_once(CABACContext *c){
    int shift = (uint32_t)(c->range - 0x100)>>31;
    c->range <<= shift;
    c->low <<= shift;
    if (!(c->low & CABAC_MASK))
        refill(c);
}

static int get_cabac_terminate(CABACContext *c){
    c->range -= 2;
    if (c->low < c->range<<(CABAC_BITS+1)) {
        renorm_cabac_decoder_once(c);
        return 0;
    } else {
        return (int)(c->bytestream - c->bytestream_start);
    }
}

/* ---- context init, spec 9.3.1.1 ---- */
static uint8_t init_ctx(int m, int n, int qp){
    int pre = (m*qp)>>4;
    pre += n;
    if (pre < 1) pre = 1;
    if (pre > 126) pre = 126;
    if (pre <= 63) return (uint8_t)(63 - pre); /* mps=0 encoded via caller's separate array */
    return (uint8_t)(pre - 64);
}

int main(int argc, char **argv){
    if (argc < 2) { fprintf(stderr, "usage: harness <payload.bin>\n"); return 1; }
    FILE *f = fopen(argv[1], "rb");
    if (!f) { perror("fopen"); return 1; }
    uint8_t buf[8192];
    size_t n = fread(buf, 1, sizeof(buf), f);
    fclose(f);

    CABACContext c;
    ff_init_cabac_decoder(&c, buf, (int)n);

    int qp = 14;

    /* mb_type bin0: ctxIdx 3, (m,n)=(20,-15), mps derived separately */
    /* init: pre = clip3(1,126, (20*14>>4) + (-15)) = 2 -> pre<=63 -> state=61,mps=0 */
    uint8_t mb_type_state = init_ctx(20, -15, qp);
    int mb_type_mps = 0; /* pre=2 <=63 => mps=0 */
    /* get_cabac_inline expects *state to encode BOTH state and mps in a single
     * byte per FFmpeg's convention: even state = mps 0, the mlps_state table
     * and lps_range table are indexed on a combined "state" where bit0 encodes
     * something else -- actually ff's uint8_t state directly *is* our
     * (state<<1)|mps encoding per h264_cabac_tables.h ff_h264_cabac_tables
     * layout (the lps_range table has doubled entries because state is
     * pre-multiplied by 2 with mps folded in as the LSB). Replicate that: */
    uint8_t state_byte = (uint8_t)((mb_type_state << 1) | mb_type_mps);

    int bin0 = get_cabac(&c, &state_byte);
    printf("mb_type bin0 = %d  range=%d low=%d bytestream_off=%ld\n",
           bin0, c.range, c.low, (long)(c.bytestream - c.bytestream_start));

    /* intra4x4 pred mode: ctxIdx 68 (prev), 69 (rem) */
    uint8_t prev_st = init_ctx(13, 41, qp); int prev_mps = 0; /* pre=52<=63 mps0 */
    uint8_t rem_st  = init_ctx(3, 62, qp);  int rem_mps  = 1; /* pre=(3*14>>4)+62=64 -> >63 mps1, state=0 */
    /* recompute properly: */
    {
        int pre = (13*qp>>4) + 41; if (pre<1) pre=1; if(pre>126) pre=126;
        if (pre<=63) { prev_st=(uint8_t)(63-pre); prev_mps=0; } else { prev_st=(uint8_t)(pre-64); prev_mps=1; }
    }
    {
        int pre = (3*qp>>4) + 62; if (pre<1) pre=1; if(pre>126) pre=126;
        if (pre<=63) { rem_st=(uint8_t)(63-pre); rem_mps=0; } else { rem_st=(uint8_t)(pre-64); rem_mps=1; }
    }
    uint8_t prev_byte = (uint8_t)((prev_st<<1)|prev_mps);
    uint8_t rem_byte  = (uint8_t)((rem_st<<1)|rem_mps);

    int n_pred = 0, n_rem = 0;
    for (int i = 0; i < 16; i++) {
        if (get_cabac(&c, &prev_byte) == 1) { n_pred++; }
        else { get_cabac(&c,&rem_byte); get_cabac(&c,&rem_byte); get_cabac(&c,&rem_byte); n_rem++; }
    }
    printf("intra4x4 done: pred_count=%d rem_count=%d range=%d low=%d off=%ld\n",
           n_pred, n_rem, c.range, c.low, (long)(c.bytestream-c.bytestream_start));

    /* intra_chroma_pred_mode: ctxIdx 64..67, ctx0 (no neighbors) */
    uint8_t chroma_st[4]; uint8_t chroma_mps[4];
    int chroma_mn[4][2] = {{-9,83},{4,86},{0,97},{-7,72}};
    uint8_t chroma_byte[4];
    for (int i=0;i<4;i++){
        int m=chroma_mn[i][0], nn=chroma_mn[i][1];
        int pre=(m*qp>>4)+nn; if(pre<1)pre=1; if(pre>126)pre=126;
        if(pre<=63){chroma_st[i]=(uint8_t)(63-pre); chroma_mps[i]=0;} else {chroma_st[i]=(uint8_t)(pre-64); chroma_mps[i]=1;}
        chroma_byte[i]=(uint8_t)((chroma_st[i]<<1)|chroma_mps[i]);
    }
    int chroma_mode;
    if (get_cabac(&c, &chroma_byte[0]) == 0) chroma_mode = 0;
    else if (get_cabac(&c, &chroma_byte[3]) == 0) chroma_mode = 1;
    else if (get_cabac(&c, &chroma_byte[3]) == 0) chroma_mode = 2;
    else chroma_mode = 3;
    printf("chroma_pred_mode = %d range=%d low=%d off=%ld\n", chroma_mode, c.range, c.low, (long)(c.bytestream-c.bytestream_start));

    /* coded_block_pattern: ctxIdx 73..84 */
    int cbp_mn[12][2] = {
        {-17,127},{-13,102},{0,82},{-7,74},{-21,107},{-27,127},{-31,127},{-24,127},
        {-18,95},{-27,127},{-21,114},{-30,127}
    };
    uint8_t cbp_byte[12];
    for (int i=0;i<12;i++){
        int m=cbp_mn[i][0], nn=cbp_mn[i][1];
        int pre=(m*qp>>4)+nn; if(pre<1)pre=1; if(pre>126)pre=126;
        uint8_t st,mp;
        if(pre<=63){st=(uint8_t)(63-pre); mp=0;} else {st=(uint8_t)(pre-64); mp=1;}
        cbp_byte[i]=(uint8_t)((st<<1)|mp);
    }
    int left_cbp = 0x7CF, top_cbp = 0x7CF;
    int cbp = 0;
    int ctxi;
    ctxi = (!(left_cbp&0x02)) + 2*(!(top_cbp&0x04));
    cbp += get_cabac(&c, &cbp_byte[ctxi]) << 0;
    ctxi = (!(cbp&0x01)) + 2*(!(top_cbp&0x08));
    cbp += get_cabac(&c, &cbp_byte[ctxi]) << 1;
    ctxi = (!(left_cbp&0x08)) + 2*(!(cbp&0x01));
    cbp += get_cabac(&c, &cbp_byte[ctxi]) << 2;
    ctxi = (!(cbp&0x04)) + 2*(!(cbp&0x02));
    cbp += get_cabac(&c, &cbp_byte[ctxi]) << 3;
    int cbp_luma = cbp;
    int cbp_a_chroma = (left_cbp>>4)&3, cbp_b_chroma=(top_cbp>>4)&3;
    int presence_ctx = (cbp_a_chroma>0) + 2*(cbp_b_chroma>0);
    int cbp_chroma;
    if (get_cabac(&c, &cbp_byte[4+presence_ctx]) == 0) cbp_chroma = 0;
    else {
        int value_ctx = (cbp_a_chroma==2) + 2*(cbp_b_chroma==2);
        cbp_chroma = 1 + get_cabac(&c, &cbp_byte[8+value_ctx]);
    }
    printf("cbp_luma=0x%x cbp_chroma=%d range=%d low=%d off=%ld\n", cbp_luma, cbp_chroma, c.range, c.low, (long)(c.bytestream-c.bytestream_start));

    /* mb_qp_delta: ctxIdx 60..63 */
    int qpd_mn[4][2] = {{0,41},{0,63},{0,63},{0,63}};
    uint8_t qpd_byte[4];
    for (int i=0;i<4;i++){
        int m=qpd_mn[i][0], nn=qpd_mn[i][1];
        int pre=(m*qp>>4)+nn; if(pre<1)pre=1; if(pre>126)pre=126;
        uint8_t st,mp;
        if(pre<=63){st=(uint8_t)(63-pre); mp=0;} else {st=(uint8_t)(pre-64); mp=1;}
        qpd_byte[i]=(uint8_t)((st<<1)|mp);
    }
    int dqp = 0;
    if (cbp_luma != 0 || cbp_chroma != 0) {
        if (get_cabac(&c, &qpd_byte[0]) == 0) dqp = 0;
        else {
            int val=1, ci=2;
            while (1) {
                if (get_cabac(&c, &qpd_byte[ci]) == 0) break;
                ci = 3; val++;
            }
            dqp = (val%2==1) ? (val+1)/2 : -((val+1)/2);
        }
    }
    printf("mb_qp_delta=%d range=%d low=%d off=%ld\n", dqp, c.range, c.low, (long)(c.bytestream-c.bytestream_start));

    /* residual: block0, Luma4x4 (cat=2), left=top=coded (unavailable+intra) */
    int cbf_mn[4][2] = {{-3,70},{-8,93},{-10,90},{-30,127}};
    uint8_t cbf_byte[4];
    for (int i=0;i<4;i++){
        int m=cbf_mn[i][0], nn=cbf_mn[i][1];
        int pre=(m*qp>>4)+nn; if(pre<1)pre=1; if(pre>126)pre=126;
        uint8_t st,mp;
        if(pre<=63){st=(uint8_t)(63-pre); mp=0;} else {st=(uint8_t)(pre-64); mp=1;}
        cbf_byte[i]=(uint8_t)((st<<1)|mp);
    }
    int coded = get_cabac(&c, &cbf_byte[3]);
    printf("block0 cbf=%d range=%d low=%d off=%ld\n", coded, c.range, c.low, (long)(c.bytestream-c.bytestream_start));

    int sig_mn[15][2] = {{-13,108},{-15,100},{-13,101},{-13,91},{-12,94},{-10,88},{-16,84},{-10,86},
                         {-7,83},{-13,87},{-19,94},{1,70},{0,72},{-5,74},{18,59}};
    int last_mn[15][2] = {{26,-19},{22,-17},{26,-17},{30,-25},{28,-20},{33,-23},{37,-27},{33,-23},
                          {40,-28},{38,-17},{33,-11},{40,-15},{41,-6},{38,1},{41,17}};
    int level_mn[10][2] = {{-12,92},{-15,55},{-10,60},{-6,62},{-4,65},{-12,73},{-8,76},{-7,80},{-9,88},{-17,110}};
    uint8_t sig_byte[15], last_byte[15], level_byte[10];
    for (int i=0;i<15;i++){
        int m=sig_mn[i][0], nn=sig_mn[i][1];
        int pre=(m*qp>>4)+nn; if(pre<1)pre=1; if(pre>126)pre=126;
        uint8_t st,mp; if(pre<=63){st=(uint8_t)(63-pre);mp=0;}else{st=(uint8_t)(pre-64);mp=1;}
        sig_byte[i]=(uint8_t)((st<<1)|mp);
    }
    for (int i=0;i<15;i++){
        int m=last_mn[i][0], nn=last_mn[i][1];
        int pre=(m*qp>>4)+nn; if(pre<1)pre=1; if(pre>126)pre=126;
        uint8_t st,mp; if(pre<=63){st=(uint8_t)(63-pre);mp=0;}else{st=(uint8_t)(pre-64);mp=1;}
        last_byte[i]=(uint8_t)((st<<1)|mp);
    }
    for (int i=0;i<10;i++){
        int m=level_mn[i][0], nn=level_mn[i][1];
        int pre=(m*qp>>4)+nn; if(pre<1)pre=1; if(pre>126)pre=126;
        uint8_t st,mp; if(pre<=63){st=(uint8_t)(63-pre);mp=0;}else{st=(uint8_t)(pre-64);mp=1;}
        level_byte[i]=(uint8_t)((st<<1)|mp);
    }

    int positions[16]; int npos=0; int found_last=0;
    for (int pos=0; pos<15; pos++){
        if (get_cabac(&c, &sig_byte[pos]) == 1){
            positions[npos++]=pos;
            if (get_cabac(&c, &last_byte[pos]) == 1){ found_last=1; break; }
        }
    }
    if (!found_last) positions[npos++]=15;
    printf("block0 sig count=%d positions:", npos);
    for (int i=0;i<npos;i++) printf(" %d", positions[i]);
    printf("\n");

    static const int level1_ctx[8] = {1,2,3,4,0,0,0,0};
    static const int levelgt1_ctx[8] = {5,5,5,5,6,7,8,9};
    static const int level_trans[2][8] = {{1,2,3,3,4,5,6,7},{4,4,4,4,5,6,7,7}};
    int out[16]={0};
    int node_ctx=0;
    for (int i=npos-1;i>=0;i--){
        int pos=positions[i];
        int l1 = level1_ctx[node_ctx];
        int level_abs;
        printf("  l1 check pos=%d: before range=%d low=%d state_byte=%u off=%ld\n", pos, c.range, c.low, level_byte[l1], (long)(c.bytestream-c.bytestream_start));
        int b0 = get_cabac(&c, &level_byte[l1]);
        printf("  l1 check pos=%d: after  range=%d low=%d state_byte=%u result=%d\n", pos, c.range, c.low, level_byte[l1], b0);
        printf("  iter pos=%d node_ctx_before=%d l1=%d b0=%d", pos, node_ctx, l1, b0);
        if (b0 == 0){
            level_abs=1; node_ctx = level_trans[0][node_ctx];
        } else {
            int gt1 = levelgt1_ctx[node_ctx];
            node_ctx = level_trans[1][node_ctx];
            int abs_val=2;
            int nbits=0;
            while (abs_val<15) {
                printf("    before call%d: range=%d low=%d state_byte=%u off=%ld\n", nbits, c.range, c.low, level_byte[gt1], (long)(c.bytestream-c.bytestream_start));
                int r = get_cabac(&c,&level_byte[gt1]);
                printf("    after  call%d: range=%d low=%d state_byte=%u result=%d\n", nbits, c.range, c.low, level_byte[gt1], r);
                if (r != 1) break;
                abs_val++; nbits++;
            }
            printf(" gt1=%d nbits=%d", gt1, nbits);
            level_abs=abs_val;
        }
        printf("    sign: before range=%d low=%d off=%ld\n", c.range, c.low, (long)(c.bytestream-c.bytestream_start));
        int sign = get_cabac_bypass(&c);
        printf("    sign: after  range=%d low=%d off=%ld result=%d\n", c.range, c.low, (long)(c.bytestream-c.bytestream_start), sign);
        out[pos] = sign ? -level_abs : level_abs;
        printf(" node_ctx_after=%d abs=%d sign=%d\n", node_ctx, level_abs, sign);
    }
    printf("block0 coeffs:");
    for (int i=0;i<16;i++) printf(" %d", out[i]);
    printf("\nfinal range=%d low=%d off=%ld\n", c.range, c.low, (long)(c.bytestream-c.bytestream_start));

    return 0;
}
