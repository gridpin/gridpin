# GridPin for Java

Offline geocoding from Java over the C interface of GridPin (`gridpin-ffi`), with the Foreign Function and Memory API
of JDK 22 and later: no JNI code to compile, no network, one local file per country.

```java
import dev.gridpin.GridPin;
import java.nio.file.Path;

try (GridPin g = GridPin.open(Path.of("france.bin"))) {
    String hits = g.geocode("1 rue de la Paix, Paris", 1);      // JSON array of up to k hits, "[]" when none
    List<String> many = g.geocode(List.of("a", "b"), 1);        // one array per query, on all cores in one call
    String near = g.reverse(48.8686, 2.3305, 1);               // nearest addresses of a point
}
```

Answers are the engine's JSON, exactly what `gridpin batch -k 1` writes in `results`: `lat`, `lon`, `precision`,
`street`, `housenumber`, `commune`, `postcode`, `confidence`, `weak`, `match_level`, `match_status`. A hit with
`"weak": true` needs a check before use. Parse it with the JSON library you already have.

## Build it yourself

No package is published (no Maven Central artifact yet). From the `code/` folder of the repository, with Rust and
JDK 22 or later:

```bash
sh integrations/java/build.sh /path/to/out     # libgridpin.dylib or libgridpin.so, and the compiled classes
java --enable-native-access=ALL-UNNAMED -Dgridpin.library=/path/to/out/libgridpin.dylib -cp /path/to/out/classes:your.jar YourMain
```

The library is linked from the static library of `gridpin-ffi`, so the C interface is the one in
`gridpin-ffi/include/gridpin.h`. It is found by the system property `gridpin.library`, the environment variable
`GRIDPIN_LIBRARY`, or the loader's search path. Linux and macOS; on Windows, link `gridpin_ffi.lib` into a DLL yourself.

## Memory and threads

- Every call copies the answers out of the engine's response and frees the response before it returns: Java never holds
  memory owned by the engine. Measured with `Check leak`: 400 000 calls keep the physical footprint flat (41 to 37 MB),
  while a version that never freed the response grew from 69 to 320 MB.
- One opened sheet can be asked from several threads at once. `close()` waits for running calls; a call after it
  throws `IllegalStateException`.

## Tests

`python3 integrations/java/test/run.py` (or `make test-java`) builds the library, checks the failures a caller can meet
(missing sheet, empty and null queries, `k` below 1, a call after `close`) and compares the answers with the CLI on the
Monaco smoke sheet field for field.
