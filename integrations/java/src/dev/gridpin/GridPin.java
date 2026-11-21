package dev.gridpin;

import java.lang.foreign.Arena;
import java.lang.foreign.FunctionDescriptor;
import java.lang.foreign.Linker;
import java.lang.foreign.MemoryLayout;
import java.lang.foreign.MemorySegment;
import java.lang.foreign.StructLayout;
import java.lang.foreign.SymbolLookup;
import java.lang.foreign.ValueLayout;
import java.lang.invoke.MethodHandle;
import java.lang.invoke.VarHandle;
import java.nio.charset.StandardCharsets;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.concurrent.locks.ReentrantReadWriteLock;

/**
 * GridPin, the offline geocoder, from Java over its C interface (gridpin-ffi/include/gridpin.h), with the Foreign
 * Function and Memory API of JDK 22 and later. No JNI glue to compile: the shared library is found by the system
 * property {@code gridpin.library}, the environment variable {@code GRIDPIN_LIBRARY}, or the loader's search path.
 *
 * <p>Answers are JSON text, exactly as the engine writes them: one array of up to {@code k} hits per query, "[]" when
 * nothing matches. Each hit carries {@code weak}: check such an answer before use. A sheet opened here is read-only
 * and may be asked from several threads at once.
 */
public final class GridPin implements AutoCloseable {
    private static final int OP_LOAD = 0;
    private static final int OP_GEOCODE = 3;
    private static final int OP_REVERSE = 4;

    /** {@code GridpinText}: borrowed bytes, their length, and a null flag. */
    private static final StructLayout TEXT = MemoryLayout.structLayout(
            ValueLayout.ADDRESS.withName("data"),
            ValueLayout.JAVA_LONG.withName("len"),
            ValueLayout.JAVA_BYTE.withName("is_null"),
            MemoryLayout.paddingLayout(7));
    private static final VarHandle TEXT_DATA = TEXT.varHandle(MemoryLayout.PathElement.groupElement("data"));
    private static final VarHandle TEXT_LEN = TEXT.varHandle(MemoryLayout.PathElement.groupElement("len"));
    private static final VarHandle TEXT_NULL = TEXT.varHandle(MemoryLayout.PathElement.groupElement("is_null"));

    private static final MethodHandle ABI;
    private static final MethodHandle STATE_NEW;
    private static final MethodHandle STATE_DROP;
    private static final MethodHandle INVOKE;
    private static final MethodHandle INVOKE_K;
    private static final MethodHandle GET;
    private static final MethodHandle DROP;

    /** The C interface version this class is written for (GRIDPIN_ABI_VERSION). */
    public static final int ABI_VERSION = 2;

    static {
        Linker linker = Linker.nativeLinker();
        SymbolLookup lib = library();
        ValueLayout size = ValueLayout.JAVA_LONG;
        ValueLayout ptr = ValueLayout.ADDRESS;
        ABI = linker.downcallHandle(find(lib, "gridpin_abi_version"), FunctionDescriptor.of(ValueLayout.JAVA_INT));
        STATE_NEW = linker.downcallHandle(find(lib, "gridpin_state_new"), FunctionDescriptor.of(ptr));
        STATE_DROP = linker.downcallHandle(find(lib, "gridpin_state_drop"), FunctionDescriptor.ofVoid(ptr));
        INVOKE = linker.downcallHandle(find(lib, "gridpin_invoke"),
                FunctionDescriptor.of(ptr, ptr, ValueLayout.JAVA_INT, ptr, ptr, ptr, ptr, size));
        INVOKE_K = linker.downcallHandle(find(lib, "gridpin_invoke_k"),
                FunctionDescriptor.of(ptr, ptr, ValueLayout.JAVA_INT, size, ptr, ptr, ptr, ptr, size));
        GET = linker.downcallHandle(find(lib, "gridpin_response_get"),
                FunctionDescriptor.of(ValueLayout.JAVA_BYTE, ptr, size, ptr));
        DROP = linker.downcallHandle(find(lib, "gridpin_response_drop"), FunctionDescriptor.ofVoid(ptr));
        int abi = abiVersion();
        if (abi != ABI_VERSION) {
            throw new UnsatisfiedLinkError("gridpin: library speaks C interface " + abi + ", this class " + ABI_VERSION);
        }
    }

    private static SymbolLookup library() {
        String path = System.getProperty("gridpin.library", System.getenv("GRIDPIN_LIBRARY"));
        if (path != null && !path.isEmpty()) {
            return SymbolLookup.libraryLookup(Path.of(path), Arena.global());
        }
        return SymbolLookup.libraryLookup(System.mapLibraryName("gridpin"), Arena.global());
    }

    private static MemorySegment find(SymbolLookup lib, String name) {
        return lib.find(name).orElseThrow(() -> new UnsatisfiedLinkError("gridpin: no symbol " + name));
    }

    /** The C interface version of the loaded library. */
    public static int abiVersion() {
        try {
            return (int) ABI.invokeExact();
        } catch (Throwable t) {
            throw new IllegalStateException(t);
        }
    }

    private MemorySegment state;
    /** Calls share the read lock; close takes the write lock, so the sheet is never dropped under a running call. */
    private final ReentrantReadWriteLock lock = new ReentrantReadWriteLock();

    private GridPin(MemorySegment state) {
        this.state = state;
    }

    /** Open a country sheet (a .bin file). Throws if the file is missing or is not a sheet. */
    public static GridPin open(Path sheet) {
        MemorySegment state;
        try {
            state = (MemorySegment) STATE_NEW.invokeExact();
        } catch (Throwable t) {
            throw new IllegalStateException(t);
        }
        if (state.equals(MemorySegment.NULL)) {
            throw new OutOfMemoryError("gridpin: no state");
        }
        GridPin g = new GridPin(state);
        try {
            g.call(OP_LOAD, 0, Arrays.asList(sheet.toString()), null, null);
        } catch (RuntimeException e) {
            g.close();
            throw e;
        }
        return g;
    }

    /** Up to {@code k} hits for one address line, as a JSON array ("[]" when nothing matches). */
    public String geocode(String query, int k) {
        return geocode(Arrays.asList(query), k).get(0);
    }

    /** One JSON array per query, in order; a null query gives null. Answered on all cores in one call. */
    public List<String> geocode(List<String> queries, int k) {
        return call(OP_GEOCODE, k, queries, null, null);
    }

    /** Up to {@code k} nearest addresses of a point, as a JSON array. */
    public String reverse(double lat, double lon, int k) {
        return reverse(new double[] {lat}, new double[] {lon}, k).get(0);
    }

    /** One JSON array per point, in order. */
    public List<String> reverse(double[] lats, double[] lons, int k) {
        if (lats.length != lons.length) {
            throw new IllegalArgumentException("gridpin: " + lats.length + " latitudes, " + lons.length + " longitudes");
        }
        return call(OP_REVERSE, k, null, lats, lons);
    }

    private List<String> call(int op, int k, List<String> texts, double[] lats, double[] lons) {
        if (op != OP_LOAD && k < 1) {
            throw new IllegalArgumentException("gridpin: k must be at least 1");
        }
        lock.readLock().lock();
        try {
            if (state == null) {
                throw new IllegalStateException("gridpin: the sheet is closed");
            }
            return callOpen(state, op, k, texts, lats, lons);
        } finally {
            lock.readLock().unlock();
        }
    }

    private static List<String> callOpen(MemorySegment st, int op, int k, List<String> texts, double[] lats,
            double[] lons) {
        int n = texts != null ? texts.size() : lats.length;
        List<String> out = new ArrayList<>(n);
        if (n == 0) {
            return out;
        }
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment textArg = MemorySegment.NULL;
            MemorySegment latArg = MemorySegment.NULL;
            MemorySegment lonArg = MemorySegment.NULL;
            MemorySegment nullArg = MemorySegment.NULL;
            if (texts != null) {
                textArg = arena.allocate(TEXT, n);
                for (int i = 0; i < n; i++) {
                    MemorySegment one = textArg.asSlice(i * TEXT.byteSize(), TEXT);
                    String s = texts.get(i);
                    if (s == null) {
                        TEXT_DATA.set(one, 0L, MemorySegment.NULL);
                        TEXT_LEN.set(one, 0L, 0L);
                        TEXT_NULL.set(one, 0L, (byte) 1);
                    } else {
                        byte[] bytes = s.getBytes(StandardCharsets.UTF_8);
                        MemorySegment data = arena.allocate(Math.max(1, bytes.length));
                        MemorySegment.copy(bytes, 0, data, ValueLayout.JAVA_BYTE, 0, bytes.length);
                        TEXT_DATA.set(one, 0L, data);
                        TEXT_LEN.set(one, 0L, (long) bytes.length);
                        TEXT_NULL.set(one, 0L, (byte) 0);
                    }
                }
            } else {
                latArg = arena.allocateFrom(ValueLayout.JAVA_DOUBLE, lats);
                lonArg = arena.allocateFrom(ValueLayout.JAVA_DOUBLE, lons);
                nullArg = arena.allocate(n); // every point present: zeroed flags
            }
            MemorySegment response = op == OP_LOAD
                    ? (MemorySegment) INVOKE.invokeExact(st, op, textArg, latArg, lonArg, MemorySegment.NULL, (long) n)
                    : (MemorySegment) INVOKE_K.invokeExact(st, op, (long) k, textArg, latArg, lonArg, nullArg,
                            (long) n);
            if (response.equals(MemorySegment.NULL)) {
                throw new IllegalStateException("gridpin: the engine returned no response");
            }
            try {
                MemorySegment row = arena.allocate(TEXT);
                for (int i = 0; i < n; i++) {
                    byte status = (byte) GET.invokeExact(response, (long) i, row);
                    String value = text(row);
                    if (status == 1) {
                        throw new IllegalStateException("gridpin: " + value);
                    }
                    if (status != 0) {
                        throw new IllegalStateException("gridpin: invalid response (status " + status + ")");
                    }
                    out.add(value);
                }
            } finally {
                DROP.invokeExact(response);
            }
        } catch (RuntimeException | Error e) {
            throw e;
        } catch (Throwable t) {
            throw new IllegalStateException(t);
        }
        return out;
    }

    /** A copy of the bytes a GridpinText borrows from the response (valid only until the response is dropped). */
    private static String text(MemorySegment row) {
        if ((byte) TEXT_NULL.get(row, 0L) != 0) {
            return null;
        }
        long len = (long) TEXT_LEN.get(row, 0L);
        MemorySegment data = ((MemorySegment) TEXT_DATA.get(row, 0L)).reinterpret(len);
        return new String(data.toArray(ValueLayout.JAVA_BYTE), StandardCharsets.UTF_8);
    }

    /** Release the sheet; further calls throw. Calling it twice is harmless. */
    @Override
    public void close() {
        lock.writeLock().lock();
        try {
            if (state != null) {
                STATE_DROP.invokeExact(state);
            }
        } catch (Throwable t) {
            throw new IllegalStateException(t);
        } finally {
            state = null;
            lock.writeLock().unlock();
        }
    }
}
