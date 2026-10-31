/* C interface to the GridPin offline geocoder (crate gridpin-ffi).
 *
 * One state holds one address sheet and, optionally, a POI sheet. Every call is a batch:
 * `len` inputs in, one response with `len` rows out. Rows are JSON text owned by the response
 * and valid until gridpin_response_drop. A Rust panic never crosses this boundary: it becomes
 * an error string in the response, or a NULL response.
 */
#ifndef GRIDPIN_H
#define GRIDPIN_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define GRIDPIN_ABI_VERSION 1

/* Operations for gridpin_invoke and gridpin_invoke_k. */
#define GRIDPIN_OP_LOAD 0     /* texts: sheet path, the same in every non-null row */
#define GRIDPIN_OP_LOAD_POI 1 /* texts: POI sheet path of the same country */
#define GRIDPIN_OP_RESET 2    /* no inputs; len must be 0 or 1 */
#define GRIDPIN_OP_GEOCODE 3  /* texts: address queries */
#define GRIDPIN_OP_REVERSE 4  /* lats, lons, nulls: coordinates */

typedef struct GridpinState GridpinState;
typedef struct GridpinResponse GridpinResponse;

/* Borrowed UTF-8 bytes; is_null != 0 marks a missing value (data may then be NULL). */
typedef struct GridpinText {
    const uint8_t *data;
    size_t len;
    uint8_t is_null;
} GridpinText;

uint32_t gridpin_abi_version(void);

/* NULL only if allocation of the state failed. */
GridpinState *gridpin_state_new(void);
void gridpin_state_drop(GridpinState *state);

/* geocode / reverse rows: the best hit as a JSON object, "{}" when nothing matches. */
GridpinResponse *gridpin_invoke(const GridpinState *state, uint32_t operation, const GridpinText *texts,
                                const double *lats, const double *lons, const uint8_t *nulls, size_t len);

/* geocode / reverse rows: a JSON array of up to k hits, "[]" when nothing matches. k >= 1. Each hit
 * also carries "weak": true when the answer needs a check before use (city level, low
 * confidence, or a fuzzy street with middling confidence). */
GridpinResponse *gridpin_invoke_k(const GridpinState *state, uint32_t operation, size_t k,
                                  const GridpinText *texts, const double *lats, const double *lons,
                                  const uint8_t *nulls, size_t len);

/* Writes row `row` into *output. Returns 0 for a value, 1 if the whole call failed (*output then
 * holds the error message), 2 for an invalid response or output pointer. A null row has
 * output->is_null set. */
uint8_t gridpin_response_get(const GridpinResponse *response, size_t row, GridpinText *output);
void gridpin_response_drop(GridpinResponse *response);

#ifdef __cplusplus
}
#endif

#endif
