/* .Call glue over the GridPin C interface (gridpin.h). Every row crosses as UTF-8 text; the
 * engine answers JSON text, which R parses. Errors from the engine become R errors after the
 * response is freed, so nothing leaks on the error path. */
#include <R.h>
#include <Rinternals.h>
#include <R_ext/Rdynload.h>
#include <stdlib.h>
#include <string.h>

#include "gridpin.h"

static void gp_finalize(SEXP ptr) {
    GridpinState *state = (GridpinState *)R_ExternalPtrAddr(ptr);
    if (state) {
        gridpin_state_drop(state);
        R_ClearExternalPtr(ptr);
    }
}

static GridpinState *gp_state(SEXP ptr) {
    if (TYPEOF(ptr) != EXTPTRSXP || R_ExternalPtrAddr(ptr) == NULL)
        Rf_error("gridpin: the geocoder was closed or not created; call gp_open() again");
    return (GridpinState *)R_ExternalPtrAddr(ptr);
}

SEXP gp_new(void) {
    GridpinState *state = gridpin_state_new();
    if (state == NULL) Rf_error("gridpin: could not allocate the engine state");
    SEXP ptr = PROTECT(R_MakeExternalPtr(state, R_NilValue, R_NilValue));
    R_RegisterCFinalizerEx(ptr, gp_finalize, TRUE);
    UNPROTECT(1);
    return ptr;
}

SEXP gp_close(SEXP ptr) {
    gp_finalize(ptr);
    return R_NilValue;
}

SEXP gp_alive(SEXP ptr) {
    return Rf_ScalarLogical(TYPEOF(ptr) == EXTPTRSXP && R_ExternalPtrAddr(ptr) != NULL);
}

SEXP gp_abi_version(void) { return Rf_ScalarInteger((int)gridpin_abi_version()); }

/* The response as a character vector of n rows (NA for a null row); an engine error is raised. */
static SEXP gp_collect(GridpinResponse *response, R_xlen_t n) {
    if (response == NULL) Rf_error("gridpin: internal error in the engine (no response)");
    GridpinText out;
    if (gridpin_response_get(response, 0, &out) == 1) {
        size_t len = out.len < 2000 ? out.len : 2000;
        char message[2001];
        memcpy(message, out.data, len);
        message[len] = '\0';
        gridpin_response_drop(response);
        Rf_error("%s", message);
    }
    SEXP result = PROTECT(Rf_allocVector(STRSXP, n));
    for (R_xlen_t i = 0; i < n; i++) {
        uint8_t status = gridpin_response_get(response, (size_t)i, &out);
        if (status != 0 || out.is_null) {
            SET_STRING_ELT(result, i, NA_STRING);
        } else {
            SET_STRING_ELT(result, i, Rf_mkCharLenCE((const char *)out.data, (int)out.len, CE_UTF8));
        }
    }
    gridpin_response_drop(response);
    UNPROTECT(1);
    return result;
}

/* op: 0 load, 1 load POI, 3 geocode. k = 0 means the single-object form (load). */
SEXP gp_text_call(SEXP ptr, SEXP op, SEXP k, SEXP x) {
    GridpinState *state = gp_state(ptr);
    if (TYPEOF(x) != STRSXP) Rf_error("gridpin: expected a character vector");
    R_xlen_t n = XLENGTH(x);
    GridpinText *texts = (GridpinText *)R_alloc((size_t)(n ? n : 1), sizeof(GridpinText));
    for (R_xlen_t i = 0; i < n; i++) {
        SEXP s = STRING_ELT(x, i);
        if (s == NA_STRING) {
            texts[i].data = NULL;
            texts[i].len = 0;
            texts[i].is_null = 1;
        } else {
            const char *utf8 = Rf_translateCharUTF8(s);
            texts[i].data = (const uint8_t *)utf8;
            texts[i].len = strlen(utf8);
            texts[i].is_null = 0;
        }
    }
    int kk = Rf_asInteger(k);
    uint32_t operation = (uint32_t)Rf_asInteger(op);
    GridpinResponse *response = kk > 0
        ? gridpin_invoke_k(state, operation, (size_t)kk, texts, NULL, NULL, NULL, (size_t)n)
        : gridpin_invoke(state, operation, texts, NULL, NULL, NULL, (size_t)n);
    return gp_collect(response, n);
}

SEXP gp_reverse_call(SEXP ptr, SEXP k, SEXP lat, SEXP lon) {
    GridpinState *state = gp_state(ptr);
    if (TYPEOF(lat) != REALSXP || TYPEOF(lon) != REALSXP || XLENGTH(lat) != XLENGTH(lon))
        Rf_error("gridpin: lat and lon must be numeric vectors of the same length");
    R_xlen_t n = XLENGTH(lat);
    uint8_t *nulls = (uint8_t *)R_alloc((size_t)(n ? n : 1), 1);
    for (R_xlen_t i = 0; i < n; i++) nulls[i] = (uint8_t)(ISNAN(REAL(lat)[i]) || ISNAN(REAL(lon)[i]));
    GridpinResponse *response = gridpin_invoke_k(state, GRIDPIN_OP_REVERSE, (size_t)Rf_asInteger(k), NULL,
                                                 REAL(lat), REAL(lon), nulls, (size_t)n);
    return gp_collect(response, n);
}

SEXP gp_reset_call(SEXP ptr) {
    GridpinState *state = gp_state(ptr);
    return gp_collect(gridpin_invoke(state, GRIDPIN_OP_RESET, NULL, NULL, NULL, NULL, 0), 1);
}

static const R_CallMethodDef CallEntries[] = {
    {"gp_new", (DL_FUNC)&gp_new, 0},
    {"gp_close", (DL_FUNC)&gp_close, 1},
    {"gp_alive", (DL_FUNC)&gp_alive, 1},
    {"gp_abi_version", (DL_FUNC)&gp_abi_version, 0},
    {"gp_text_call", (DL_FUNC)&gp_text_call, 4},
    {"gp_reverse_call", (DL_FUNC)&gp_reverse_call, 4},
    {"gp_reset_call", (DL_FUNC)&gp_reset_call, 1},
    {NULL, NULL, 0}};

void R_init_gridpin(DllInfo *dll) {
    R_registerRoutines(dll, NULL, CallEntries, NULL, NULL);
    R_useDynamicSymbols(dll, FALSE);
}
