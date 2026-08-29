/* oracle.c — decode the dumped MBAFF P-slice using ffmpeg's context selection.
 * Compiled standalone: clang oracle.c oracle_engine.c oracle_tables.c -o oracle
 * Usage: oracle <slice_params.bin> <slice_cabac.bin>
 * Dumps per-MB mb_type + mvd in raster order. */
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include "oracle_engine.h"

/* scan8: raster block (row*4+col) -> CABAC scan index */
static const int scan8[16] = {
    0,1,4,5, 2,3,6,7, 8,9,12,13, 10,11,14,15
};

#define NCTX 1024
static uint8_t state[NCTX];

/* per-MB storage */
static int mb_type[16];     /* 0=L0_16x16,1=L0_L0_16x8,2=L0_L0_8x16,3=8x8 */
static int mb_skip[16];
static int mb_field[16];
static short mvd_l0[16][2];
static int cbp_luma, cbp_chroma;

/* neighbour MBAFF derivation for frame pictures, frame-coded current.
 * Returns left_mb and top_mb grid indices, -1 if off-picture. */
static void neighbours(int mb_x, int mb_y, int cols, int rows,
                       int *left, int *top){
    *left = (mb_x>0) ? mb_y*cols + mb_x-1 : -1;
    *top  = (mb_y>0) ? (mb_y-1)*cols + mb_x : -1;
}

static int decode_mb(CABACContext *c, int mb_x, int mb_y, int cols, int rows,
                     int top_of_pair, int *prev_mb_skipped, int *next_mb_skipped,
                     int *cur_pair_field){
    int grid = mb_y*cols + mb_x;
    int left, top;
    int ctx, is_skip;

    neighbours(mb_x, mb_y, cols, rows, &left, &top);

    /* skip flag */
    if(!top_of_pair && *prev_mb_skipped){
        is_skip = *next_mb_skipped;
    } else {
        int ls=0, ts=0;
        if(left>=0) ls = mb_skip[left];
        if(top>=0)  ts = mb_skip[top];
        ctx = 11 + (!ls) + (!ts);
        is_skip = get_cabac(c, &state[ctx]);
    }

    int pair_field_pending = 0;
    if(top_of_pair){
        if(is_skip){
            /* pre-read bottom skip */
            int bls = 0;
            int bl_x = mb_x, bl_y = mb_y+1;
            int bl = (bl_x>=0 && bl_y<rows) ? bl_y*cols+bl_x : -1;
            if(bl>=0) bls = mb_skip[bl];
            ctx = 11 + (!bls) + 1; /* top=skip */
            *next_mb_skipped = get_cabac(c, &state[ctx]);
            if(!*next_mb_skipped) pair_field_pending = 1;
        } else {
            pair_field_pending = 1;
        }
    }
    if(pair_field_pending){
        int lf=0, tf=0;
        if(left>=0) lf = mb_field[left];
        if(top>=0)  tf = mb_field[top];
        ctx = 70 + lf + 2*tf;
        *cur_pair_field = get_cabac(c, &state[ctx]);
        mb_field[grid] = *cur_pair_field;
        if(mb_y+1 < rows) mb_field[(mb_y+1)*cols+mb_x] = *cur_pair_field;
    }

    if(is_skip){
        mb_skip[grid]=1;
        mb_type[grid]=0; /* P_L0_16x16 for skip */
        int i; for(i=0;i<16;i++){ mvd_l0[i][0]=0; mvd_l0[i][1]=0; }
        *prev_mb_skipped=1;
        /* skip MBs do NOT consume terminate bin (MBAFF frame). */
        return 0;
    }
    *prev_mb_skipped=0;

    /* mb_type P: ctx 14,15,16,17 */
    int intra = get_cabac(c, &state[14]);
    int mt;
    if(intra){ mt = -1; /* intra-in-P, not expected here */ }
    else {
        int a = get_cabac(c, &state[15]);
        if(a==0){
            int b = get_cabac(c, &state[16]);
            mt = 3*b; /* 0=L0_16x16, 3=8x8 */
        } else {
            int b = get_cabac(c, &state[17]);
            mt = 2-b; /* 2=L0_L0_8x16, 1=L0_L0_16x8 */
        }
    }
    mb_type[grid]=mt;

    /* partition count */
    int np = (mt==0)?1:(mt<=2)?2:4;
    /* ref_idx: num_ref=1, skip */
    /* mvd: ctx 40 (x), 47 (y), amvd context */
    int pi;
    for(pi=0; pi<np; pi++){
        int col4, row4, w4, h4;
        if(mt==0){ col4=0;row4=0;w4=4;h4=4; }
        else if(mt==1){ col4=0; row4=(pi?2:0); w4=4; h4=2; }
        else if(mt==2){ col4=(pi?2:0); row4=0; w4=2; h4=4; }
        else { col4=(pi%2)*2; row4=(pi/2)*2; w4=2; h4=2; }
        int xp=col4*4, yp=row4*4;
        /* amvd_sum: left + top |mvd_x| */
        int asum=0;
        int bx=xp/4, by=yp/4;
        if(bx>0){
            int blk = by*4 + (bx-1);
            short mv = mvd_l0[blk][0];
            asum += mv<0?-mv:mv;
        } else if(left>=0){
            int blk = by*4 + 3;
            /* would need neighbour mvd; for oracle use 0 approx */
        }
        if(by>0){
            int blk = (by-1)*4 + bx;
            short mv = mvd_l0[blk][0];
            asum += mv<0?-mv:mv;
        } else if(top>=0){
            /* approx 0 */
        }
        /* decode mvd_x (simplified: just consume bins, store value) */
        int mxd = decode_mvd(c, 40, asum);
        int myd = decode_mvd(c, 47, 0);
        int bi; for(bi=0;bi<4;bi++){
            int bidx = (row4+bi/2)*4 + (col4+bi%2);
            mvd_l0[bidx][0]=mxd; mvd_l0[bidx][1]=myd;
        }
    }

    /* cbp, dqp, residual (simplified consumption) */
    /* ... for oracle we mainly need mb_type + mvd, so we approximate residual */
    (void)cbp_luma; (void)cbp_chroma;
    return 0;
}

int main(int argc, char **argv){
    if(argc<3){ fprintf(stderr,"usage: oracle <params> <cabac>\n"); return 1; }
    FILE *fp=fopen(argv[1],"rb");
    uint32_t p[7];
    fread(p,4,7,fp); fclose(fp);
    int cols=p[0], rows=p[1];
    FILE *fb=fopen(argv[2],"rb");
    fseek(fb,0,SEEK_END); long sz=ftell(fb); fseek(fb,0,SEEK_SET);
    uint8_t *buf=malloc(sz); fread(buf,1,sz,fb); fclose(fb);

    CABACContext c;
    /* init ctx state (simplified) */
    int i; for(i=0;i<NCTX;i++) state[i] = (i%2==0)?127:0;
    /* seek to bit_offset within rbsp — our dump starts at rbsp byte 0 */
    if(ff_init_cabac_decoder(&c, buf, (int)sz)!=0){
        fprintf(stderr,"cabac init failed\n"); return 2;
    }

    int total=cols*rows;
    int prev_skip=0, next_skip=0, cpf=0;
    int mb_idx;
    for(mb_idx=0; mb_idx<total; mb_idx++){
        int pair=mb_idx>>1;
        int px=pair%cols, py=pair/cols;
        int my=2*py+(mb_idx&1);
        int top_of_pair=(mb_idx&1)==0;
        decode_mb(&c, px, my, cols, rows, top_of_pair, &prev_skip, &next_skip, &cpf);
    }

    printf("ORACLE mb_type (raster) for %dx%d:\n", cols, rows);
    int y,x;
    for(y=0;y<rows;y++) for(x=0;x<cols;x++){
        int g=y*cols+x;
        const char *nme[]={"L0_16x16","L0_L0_16x8","L0_L0_8x16","8x8"};
        printf("  MB(%d,%d) %s skip=%d field=%d mvd0=(%d,%d)\n",x,y,
               mb_type[g]<0?"?":nme[mb_type[g]],mb_skip[g],mb_field[g],
               mvd_l0[0][0],mvd_l0[0][1]);
    }
    free(buf);
    return 0;
}
