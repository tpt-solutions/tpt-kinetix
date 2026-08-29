#ifndef ORACLE_ENGINE_H
#define ORACLE_ENGINE_H
#include <stdint.h>
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
int get_cabac(CABACContext *c, uint8_t * const state);
int get_cabac_bypass(CABACContext *c);
int get_cabac_terminate(CABACContext *c);
#endif
