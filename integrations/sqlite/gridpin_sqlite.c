/* GridPin as a loadable SQLite extension, over the C interface of gridpin-ffi.
 *
 *   .load ./gridpin_sqlite
 *   SELECT gridpin_open('mc.bin');                      -- once per connection: the country sheet
 *   SELECT gridpin_reset();                             -- unloads it, so gridpin_open can take another sheet
 *   SELECT gridpin_geocode('boulevard des moulins 27, monaco');   -- the best answer as JSON, '{}' for none
 *   SELECT gridpin_lat(address), gridpin_lon(address) FROM customers;
 *   SELECT json_extract(gridpin_geocode(address), '$.match_level') FROM customers;
 */
#include <sqlite3ext.h>
SQLITE_EXTENSION_INIT1
#include <stdlib.h>
#include <string.h>
#include "gridpin.h"

static GridpinState *state(sqlite3_context *ctx) { return (GridpinState *)sqlite3_user_data(ctx); }

/* The engine's messages name the functions of its DuckDB extension; here the sheet is opened by gridpin_open.
 * Both names have twelve letters, so the message is rewritten in place. */
static void sqlite_names(char *msg) {
  for (char *at = msg; (at = strstr(at, "gridpin_load")) != NULL; at += 12) memcpy(at, "gridpin_open", 12);
}

/* One call of the engine on one text; the row as a malloc'd string the caller frees. NULL either for no answer
 * or for an error; *failed tells them apart, and on an error the message is already the result, so the caller
 * must not set another one (sqlite3_result_null after sqlite3_result_error wipes the message). */
static char *call(sqlite3_context *ctx, uint32_t op, sqlite3_value *arg, int *failed) {
  *failed = 0;
  if (sqlite3_value_type(arg) == SQLITE_NULL) return NULL;
  GridpinText in = {sqlite3_value_text(arg), (size_t)sqlite3_value_bytes(arg), 0};
  GridpinResponse *r = gridpin_invoke(state(ctx), op, &in, NULL, NULL, NULL, 1);
  if (!r) { *failed = 1; sqlite3_result_error(ctx, "gridpin: the engine failed", -1); return NULL; }
  GridpinText out;
  uint8_t rc = gridpin_response_get(r, 0, &out);
  char *text = NULL;
  if (rc == 1) {
    char *msg = sqlite3_mprintf("gridpin: %.*s", (int)out.len, (const char *)out.data);
    *failed = 1;
    if (msg) { sqlite_names(msg); sqlite3_result_error(ctx, msg, -1); sqlite3_free(msg); }
    else sqlite3_result_error_nomem(ctx);
  } else if (rc == 0 && !out.is_null) {
    text = malloc(out.len + 1);
    memcpy(text, out.data, out.len);
    text[out.len] = 0;
  }
  gridpin_response_drop(r);
  return text;
}

static void f_open(sqlite3_context *ctx, int argc, sqlite3_value **argv) {
  (void)argc;
  int failed;
  char *t = call(ctx, GRIDPIN_OP_LOAD, argv[0], &failed);
  if (t) { sqlite3_result_text(ctx, t, -1, SQLITE_TRANSIENT); free(t); } else if (!failed) sqlite3_result_null(ctx);
}

static void f_reset(sqlite3_context *ctx, int argc, sqlite3_value **argv) {
  (void)argc; (void)argv;
  GridpinResponse *r = gridpin_invoke(state(ctx), GRIDPIN_OP_RESET, NULL, NULL, NULL, NULL, 0);
  if (!r) { sqlite3_result_error(ctx, "gridpin: the engine failed", -1); return; }
  gridpin_response_drop(r);
  sqlite3_result_text(ctx, "index unloaded", -1, SQLITE_STATIC);
}

static void f_geocode(sqlite3_context *ctx, int argc, sqlite3_value **argv) {
  (void)argc;
  int failed;
  char *t = call(ctx, GRIDPIN_OP_GEOCODE, argv[0], &failed);
  if (t) { sqlite3_result_text(ctx, t, -1, SQLITE_TRANSIENT); free(t); } else if (!failed) sqlite3_result_null(ctx);
}

/* "lat" or "lon" read from the JSON row: the numbers follow "lat": and "lon": in it. */
static void coordinate(sqlite3_context *ctx, sqlite3_value **argv, const char *key) {
  int failed;
  char *t = call(ctx, GRIDPIN_OP_GEOCODE, argv[0], &failed);
  if (!t) { if (!failed) sqlite3_result_null(ctx); return; }
  char *at = strstr(t, key);
  if (at) sqlite3_result_double(ctx, strtod(at + strlen(key), NULL)); else sqlite3_result_null(ctx);
  free(t);
}

static void f_lat(sqlite3_context *ctx, int argc, sqlite3_value **argv) { (void)argc; coordinate(ctx, argv, "\"lat\":"); }
static void f_lon(sqlite3_context *ctx, int argc, sqlite3_value **argv) { (void)argc; coordinate(ctx, argv, "\"lon\":"); }

static void drop_state(void *p) { gridpin_state_drop((GridpinState *)p); }

#ifdef _WIN32
__declspec(dllexport)
#endif
int sqlite3_gridpinsqlite_init(sqlite3 *db, char **err, const sqlite3_api_routines *api) {
  SQLITE_EXTENSION_INIT2(api);
  (void)err;
  GridpinState *s = gridpin_state_new();
  if (!s) return SQLITE_NOMEM;
  int flags = SQLITE_UTF8 | SQLITE_DETERMINISTIC;
  int rc = sqlite3_create_function_v2(db, "gridpin_open", 1, SQLITE_UTF8, s, f_open, NULL, NULL, drop_state);
  if (rc == SQLITE_OK) rc = sqlite3_create_function_v2(db, "gridpin_reset", 0, SQLITE_UTF8, s, f_reset, NULL, NULL, NULL);
  if (rc == SQLITE_OK) rc = sqlite3_create_function_v2(db, "gridpin_geocode", 1, flags, s, f_geocode, NULL, NULL, NULL);
  if (rc == SQLITE_OK) rc = sqlite3_create_function_v2(db, "gridpin_lat", 1, flags, s, f_lat, NULL, NULL, NULL);
  if (rc == SQLITE_OK) rc = sqlite3_create_function_v2(db, "gridpin_lon", 1, flags, s, f_lon, NULL, NULL, NULL);
  return rc;
}
