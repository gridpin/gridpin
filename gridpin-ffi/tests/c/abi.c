/* Drives include/gridpin.h against the static library: built and run by tests/c_abi.rs.
 * Usage: abi <sheet.bin>. Prints the failed check and exits non-zero on the first failure. */
#include "gridpin.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int failures = 0;
#define CHECK(cond, what)                                   \
    do {                                                    \
        if (!(cond)) {                                      \
            fprintf(stderr, "FAILED: %s\n", what);          \
            failures++;                                     \
        }                                                   \
    } while (0)

static GridpinText text(const char *s) {
    GridpinText t = {(const uint8_t *)s, s ? strlen(s) : 0, (uint8_t)(s == NULL)};
    return t;
}

/* Row `row` as a NUL-terminated copy (or NULL for a null row); *status gets the get() code. */
static char *row_of(GridpinResponse *r, size_t row, int *status) {
    GridpinText out = {0};
    *status = gridpin_response_get(r, row, &out);
    if (*status == 2 || out.is_null) return NULL;
    char *s = malloc(out.len + 1);
    memcpy(s, out.data, out.len);
    s[out.len] = 0;
    return s;
}

static int contains(const char *s, const char *needle) { return s && strstr(s, needle) != NULL; }

int main(int argc, char **argv) {
    if (argc != 2) return 64;
    int st;
    char *s;
    CHECK(gridpin_abi_version() == GRIDPIN_ABI_VERSION, "abi version matches the header");

    GridpinState *state = gridpin_state_new();
    CHECK(state != NULL, "state allocated");

    GridpinText q[2] = {text("1 rue a, ville"), text(NULL)};
    GridpinResponse *r = gridpin_invoke(state, GRIDPIN_OP_GEOCODE, q, NULL, NULL, NULL, 2);
    s = row_of(r, 0, &st);
    CHECK(st == 1 && contains(s, "call gridpin_load"), "geocode before load is an error with a hint");
    free(s);
    gridpin_response_drop(r);

    GridpinText path = text(argv[1]);
    r = gridpin_invoke(state, GRIDPIN_OP_LOAD, &path, NULL, NULL, NULL, 1);
    s = row_of(r, 0, &st);
    CHECK(st == 0 && contains(s, "index loaded"), "sheet loads");
    free(s);
    gridpin_response_drop(r);

    r = gridpin_invoke(state, GRIDPIN_OP_GEOCODE, q, NULL, NULL, NULL, 2);
    s = row_of(r, 0, &st);
    CHECK(st == 0 && s && s[0] == '{' && contains(s, "\"precision\":\"house\""), "geocode row is one JSON object");
    free(s);
    s = row_of(r, 1, &st);
    CHECK(st == 0 && s == NULL, "a null query gives a null row");
    gridpin_response_drop(r);

    r = gridpin_invoke_k(state, GRIDPIN_OP_GEOCODE, 3, q, NULL, NULL, NULL, 1);
    s = row_of(r, 0, &st);
    CHECK(st == 0 && s && s[0] == '[' && contains(s, "\"precision\":\"house\""), "geocode with k is a JSON array");
    CHECK(contains(s, "\"weak\":false"), "a door-level match with k carries weak = false");
    free(s);
    gridpin_response_drop(r);

    GridpinText nowhere = text("zzzz qqqq");
    r = gridpin_invoke(state, GRIDPIN_OP_GEOCODE, &nowhere, NULL, NULL, NULL, 1);
    s = row_of(r, 0, &st);
    CHECK(st == 0 && s && strcmp(s, "{}") == 0, "no match is {} without k");
    free(s);
    gridpin_response_drop(r);
    r = gridpin_invoke_k(state, GRIDPIN_OP_GEOCODE, 2, &nowhere, NULL, NULL, NULL, 1);
    s = row_of(r, 0, &st);
    CHECK(st == 0 && s && strcmp(s, "[]") == 0, "no match is [] with k");
    free(s);
    gridpin_response_drop(r);

    GridpinText town = text("ville");
    r = gridpin_invoke_k(state, GRIDPIN_OP_GEOCODE, 1, &town, NULL, NULL, NULL, 1);
    s = row_of(r, 0, &st);
    CHECK(st == 0 && contains(s, "\"precision\":\"city\"") && contains(s, "\"weak\":true"),
          "a town-level answer with k carries weak = true");
    free(s);
    gridpin_response_drop(r);
    r = gridpin_invoke(state, GRIDPIN_OP_GEOCODE, q, NULL, NULL, NULL, 1);
    s = row_of(r, 0, &st);
    CHECK(st == 0 && !contains(s, "weak"), "the SQL form carries no weak field");
    free(s);
    gridpin_response_drop(r);

    r = gridpin_invoke_k(state, GRIDPIN_OP_GEOCODE, 0, q, NULL, NULL, NULL, 1);
    s = row_of(r, 0, &st);
    CHECK(st == 1 && contains(s, "k must be at least 1"), "k = 0 is refused");
    free(s);
    gridpin_response_drop(r);

    double lat = 43.73, lon = 7.42;
    uint8_t not_null = 0;
    r = gridpin_invoke(state, GRIDPIN_OP_REVERSE, NULL, &lat, &lon, &not_null, 1);
    s = row_of(r, 0, &st);
    CHECK(st == 0 && contains(s, "distance_m"), "reverse returns the nearest house");
    free(s);
    gridpin_response_drop(r);

    r = gridpin_invoke(state, 9, q, NULL, NULL, NULL, 1);
    s = row_of(r, 0, &st);
    CHECK(st == 1 && contains(s, "unknown GridPin operation"), "an unknown operation is an error");
    free(s);
    gridpin_response_drop(r);

    r = gridpin_invoke(NULL, GRIDPIN_OP_GEOCODE, q, NULL, NULL, NULL, 1);
    s = row_of(r, 0, &st);
    CHECK(st == 1 && contains(s, "missing GridPin state"), "a null state is an error, not a crash");
    free(s);
    gridpin_response_drop(r);

    GridpinText out;
    CHECK(gridpin_response_get(NULL, 0, &out) == 2, "a null response reads as invalid");

    gridpin_state_drop(state);
    gridpin_state_drop(NULL);
    gridpin_response_drop(NULL);
    if (failures == 0) puts("OK");
    return failures == 0 ? 0 : 1;
}
