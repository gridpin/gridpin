/* Node-API glue over the GridPin C interface (gridpin.h), the same interface the R package and the DuckDB
 * extension use. Every row crosses as UTF-8 text; the engine answers JSON text, which index.js parses.
 * An engine error becomes a JavaScript exception after the response is freed, so nothing leaks. */
#define NAPI_VERSION 8
#include <node_api.h>
#include <stdlib.h>
#include <string.h>

#include "gridpin.h"

#define CHECK(call)                                                                 \
    do {                                                                            \
        if ((call) != napi_ok) {                                                    \
            napi_throw_error(env, NULL, "gridpin: Node-API call failed: " #call);   \
            return NULL;                                                            \
        }                                                                           \
    } while (0)

static void finalize(napi_env env, void *data, void *hint) {
    (void)env;
    (void)hint;
    if (data) gridpin_state_drop((GridpinState *)data);
}

/* state(): a new engine state wrapped as an external; freed by the garbage collector or close(). */
static napi_value state_new(napi_env env, napi_callback_info info) {
    (void)info;
    GridpinState *state = gridpin_state_new();
    if (state == NULL) {
        napi_throw_error(env, NULL, "gridpin: could not allocate the engine state");
        return NULL;
    }
    napi_value out;
    CHECK(napi_create_external(env, state, finalize, NULL, &out));
    return out;
}

static GridpinState *state_of(napi_env env, napi_value value) {
    void *data = NULL;
    if (napi_get_value_external(env, value, &data) != napi_ok || data == NULL) {
        napi_throw_error(env, NULL, "gridpin: the geocoder was closed or not created; open it again");
        return NULL;
    }
    return (GridpinState *)data;
}

/* The response as an array of n strings (null for a null row); an engine error is thrown. */
static napi_value collect(napi_env env, GridpinResponse *response, size_t n) {
    if (response == NULL) {
        napi_throw_error(env, NULL, "gridpin: internal error in the engine (no response)");
        return NULL;
    }
    GridpinText out;
    if (gridpin_response_get(response, 0, &out) == 1) {
        size_t len = out.len < 2000 ? out.len : 2000;
        char message[2001];
        memcpy(message, out.data, len);
        message[len] = '\0';
        gridpin_response_drop(response);
        napi_throw_error(env, NULL, message);
        return NULL;
    }
    napi_value result, item;
    if (napi_create_array_with_length(env, n, &result) != napi_ok) {
        gridpin_response_drop(response);
        napi_throw_error(env, NULL, "gridpin: cannot allocate the result");
        return NULL;
    }
    for (size_t i = 0; i < n; i++) {
        uint8_t status = gridpin_response_get(response, i, &out);
        napi_status made = (status != 0 || out.is_null)
            ? napi_get_null(env, &item)
            : napi_create_string_utf8(env, (const char *)out.data, out.len, &item);
        if (made != napi_ok || napi_set_element(env, result, (uint32_t)i, item) != napi_ok) {
            gridpin_response_drop(response);
            napi_throw_error(env, NULL, "gridpin: cannot build the result");
            return NULL;
        }
    }
    gridpin_response_drop(response);
    return result;
}

/* The optional `with` argument: NULL when absent, undefined, null or empty; a malloc'd copy otherwise
 * (*bad is set when it is there but not a string). */
static char *with_of(napi_env env, size_t argc, napi_value *argv, size_t at, int *bad) {
    *bad = 0;
    if (argc <= at) return NULL;
    napi_valuetype type;
    if (napi_typeof(env, argv[at], &type) != napi_ok || type == napi_undefined || type == napi_null) return NULL;
    if (type != napi_string) {
        *bad = 1;
        return NULL;
    }
    size_t len = 0;
    napi_get_value_string_utf8(env, argv[at], NULL, 0, &len);
    if (len == 0) return NULL;
    char *s = malloc(len + 1);
    if (s) napi_get_value_string_utf8(env, argv[at], s, len + 1, &len);
    return s;
}

/* text(state, op, k, rows[, with]): op 0 load, 1 load POI, 3 geocode; k = 0 is the single-object form;
 * `with` (extra answer fields, gridpin batch --with) needs k >= 1. */
static napi_value text_call(napi_env env, napi_callback_info info) {
    size_t argc = 5;
    napi_value argv[5];
    CHECK(napi_get_cb_info(env, info, &argc, argv, NULL, NULL));
    if (argc != 4 && argc != 5) {
        napi_throw_type_error(env, NULL, "gridpin: text(state, op, k, rows[, with])");
        return NULL;
    }
    GridpinState *state = state_of(env, argv[0]);
    if (state == NULL) return NULL;
    uint32_t op, k, n;
    CHECK(napi_get_value_uint32(env, argv[1], &op));
    CHECK(napi_get_value_uint32(env, argv[2], &k));
    bool is_array = false;
    CHECK(napi_is_array(env, argv[3], &is_array));
    if (!is_array) {
        napi_throw_type_error(env, NULL, "gridpin: expected an array of strings");
        return NULL;
    }
    CHECK(napi_get_array_length(env, argv[3], &n));
    GridpinText *texts = calloc(n ? n : 1, sizeof(GridpinText));
    char **owned = calloc(n ? n : 1, sizeof(char *));
    napi_value result = NULL;
    if (texts == NULL || owned == NULL) {
        napi_throw_error(env, NULL, "gridpin: out of memory");
        goto done;
    }
    for (uint32_t i = 0; i < n; i++) {
        napi_value row;
        napi_valuetype type;
        if (napi_get_element(env, argv[3], i, &row) != napi_ok || napi_typeof(env, row, &type) != napi_ok) {
            napi_throw_error(env, NULL, "gridpin: cannot read the input");
            goto done;
        }
        if (type == napi_null || type == napi_undefined) {
            texts[i].is_null = 1;
            continue;
        }
        if (type != napi_string) {
            napi_throw_type_error(env, NULL, "gridpin: every row must be a string or null");
            goto done;
        }
        size_t len = 0;
        napi_get_value_string_utf8(env, row, NULL, 0, &len);
        owned[i] = malloc(len + 1);
        if (owned[i] == NULL) {
            napi_throw_error(env, NULL, "gridpin: out of memory");
            goto done;
        }
        napi_get_value_string_utf8(env, row, owned[i], len + 1, &len);
        texts[i].data = (const uint8_t *)owned[i];
        texts[i].len = len;
    }
    int bad = 0;
    char *with = with_of(env, argc, argv, 4, &bad);
    if (bad || (with && k == 0)) {
        free(with);
        napi_throw_type_error(env, NULL, "gridpin: with must be a string of field names, with k >= 1");
        goto done;
    }
    GridpinText with_text = {(const uint8_t *)with, with ? strlen(with) : 0, (uint8_t)(with == NULL)};
    GridpinResponse *response = with
        ? gridpin_invoke_with(state, op, k, &with_text, texts, NULL, NULL, NULL, n)
        : k > 0
        ? gridpin_invoke_k(state, op, k, texts, NULL, NULL, NULL, n)
        : gridpin_invoke(state, op, texts, NULL, NULL, NULL, n);
    free(with);
    result = collect(env, response, n);
done:
    if (owned)
        for (uint32_t i = 0; i < n; i++) free(owned[i]);
    free(owned);
    free(texts);
    return result;
}

/* reverse(state, k, lats, lons[, with]): NaN in either is a null row. */
static napi_value reverse_call(napi_env env, napi_callback_info info) {
    size_t argc = 5;
    napi_value argv[5];
    CHECK(napi_get_cb_info(env, info, &argc, argv, NULL, NULL));
    if (argc != 4 && argc != 5) {
        napi_throw_type_error(env, NULL, "gridpin: reverse(state, k, lats, lons[, with])");
        return NULL;
    }
    GridpinState *state = state_of(env, argv[0]);
    if (state == NULL) return NULL;
    uint32_t k, n, m;
    CHECK(napi_get_value_uint32(env, argv[1], &k));
    CHECK(napi_get_array_length(env, argv[2], &n));
    CHECK(napi_get_array_length(env, argv[3], &m));
    if (n != m || k == 0) {
        napi_throw_range_error(env, NULL, "gridpin: lats and lons must have the same length, k >= 1");
        return NULL;
    }
    double *lats = calloc(n ? n : 1, sizeof(double)), *lons = calloc(n ? n : 1, sizeof(double));
    uint8_t *nulls = calloc(n ? n : 1, 1);
    napi_value result = NULL;
    if (lats == NULL || lons == NULL || nulls == NULL) {
        napi_throw_error(env, NULL, "gridpin: out of memory");
        goto done;
    }
    for (uint32_t i = 0; i < n; i++) {
        napi_value a, b;
        if (napi_get_element(env, argv[2], i, &a) != napi_ok || napi_get_element(env, argv[3], i, &b) != napi_ok ||
            napi_get_value_double(env, a, &lats[i]) != napi_ok || napi_get_value_double(env, b, &lons[i]) != napi_ok) {
            napi_throw_type_error(env, NULL, "gridpin: coordinates must be numbers");
            goto done;
        }
        nulls[i] = (uint8_t)(lats[i] != lats[i] || lons[i] != lons[i]);
    }
    int bad = 0;
    char *with = with_of(env, argc, argv, 4, &bad);
    if (bad) {
        napi_throw_type_error(env, NULL, "gridpin: with must be a string of field names");
        goto done;
    }
    GridpinText with_text = {(const uint8_t *)with, with ? strlen(with) : 0, (uint8_t)(with == NULL)};
    result = collect(env,
                     with ? gridpin_invoke_with(state, GRIDPIN_OP_REVERSE, k, &with_text, NULL, lats, lons, nulls, n)
                          : gridpin_invoke_k(state, GRIDPIN_OP_REVERSE, k, NULL, lats, lons, nulls, n),
                     n);
    free(with);
done:
    free(lats);
    free(lons);
    free(nulls);
    return result;
}

static napi_value abi(napi_env env, napi_callback_info info) {
    (void)info;
    napi_value out;
    CHECK(napi_create_uint32(env, gridpin_abi_version(), &out));
    return out;
}

static napi_value init(napi_env env, napi_value exports) {
    napi_property_descriptor props[] = {
        {"state", NULL, state_new, NULL, NULL, NULL, napi_default, NULL},
        {"text", NULL, text_call, NULL, NULL, NULL, napi_default, NULL},
        {"reverse", NULL, reverse_call, NULL, NULL, NULL, napi_default, NULL},
        {"abi", NULL, abi, NULL, NULL, NULL, napi_default, NULL},
    };
    if (napi_define_properties(env, exports, sizeof props / sizeof props[0], props) != napi_ok) return NULL;
    return exports;
}

NAPI_MODULE(NODE_GYP_MODULE_NAME, init)
