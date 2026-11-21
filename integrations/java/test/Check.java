import dev.gridpin.GridPin;

import java.io.BufferedWriter;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;

/**
 * Test driver of the Java wrapper, run by tools/../integrations/java/test/run.py:
 *   check <sheet> queries <in.txt> <out.jsonl>   one query per line in, one JSON array per line out (k = 1)
 *   check <sheet> errors                         the failures a caller can meet, each must throw or answer "[]"
 *   check <sheet> leak <n>                       n single calls; prints the resident memory before and after (KB)
 */
public final class Check {
    public static void main(String[] args) throws Exception {
        Path sheet = Path.of(args[0]);
        switch (args[1]) {
            case "queries" -> {
                List<String> qs = Files.readAllLines(Path.of(args[2]), StandardCharsets.UTF_8);
                try (GridPin g = GridPin.open(sheet); BufferedWriter w = Files.newBufferedWriter(Path.of(args[3]))) {
                    for (int i = 0; i < qs.size(); i += 1000) {
                        for (String row : g.geocode(qs.subList(i, Math.min(qs.size(), i + 1000)), 1)) {
                            w.write(row);
                            w.write('\n');
                        }
                    }
                }
            }
            case "errors" -> {
                expectThrow("missing sheet", () -> GridPin.open(Path.of("/nonexistent/x.bin")));
                try (GridPin g = GridPin.open(sheet)) {
                    say("empty query", g.geocode("", 1));
                    say("nonsense", g.geocode("zzzz qqqq", 1));
                    List<String> withNull = new ArrayList<>();
                    withNull.add(null);
                    say("null query", String.valueOf(g.geocode(withNull, 1).get(0)));
                    expectThrow("k = 0", () -> g.geocode("x", 0));
                    say("reverse at 0,0", g.reverse(0, 0, 1));
                    expectThrow("lats and lons of different length", () -> g.reverse(new double[] {1}, new double[] {}, 1));
                    g.close();
                    g.close();
                    expectThrow("after close", () -> g.geocode("x", 1));
                }
            }
            case "leak" -> {
                int n = Integer.parseInt(args[2]);
                try (GridPin g = GridPin.open(sheet)) {
                    for (int i = 0; i < n / 10; i++) {
                        g.geocode("boulevard des moulins 27 monaco", 1);
                    }
                    // resident memory after each quarter, heap collected first: a native leak grows step by step,
                    // the JVM's own heap settles
                    StringBuilder steps = new StringBuilder();
                    for (int q = 0; q <= 4; q++) {
                        System.gc();
                        steps.append(q == 0 ? "" : " ").append(rss());
                        for (int i = 0; q < 4 && i < n / 4; i++) {
                            g.geocode("boulevard des moulins 27 monaco", 1);
                        }
                    }
                    System.out.println("rss_kb_by_quarter=" + steps + " calls=" + n);
                }
            }
            default -> throw new IllegalArgumentException(args[1]);
        }
    }

    /** Memory the process holds, in KB. On macOS the physical footprint (vmmap): resident size misses pages the
     *  system has compressed, and a leak of untouched blocks is exactly that; elsewhere the resident size. */
    private static long rss() throws Exception {
        String pid = Long.toString(ProcessHandle.current().pid());
        if (System.getProperty("os.name").startsWith("Mac")) {
            Process p = new ProcessBuilder("vmmap", "-summary", pid).redirectErrorStream(true).start();
            for (String line : new String(p.getInputStream().readAllBytes()).split("\n")) {
                if (line.startsWith("Physical footprint:")) {
                    String v = line.substring(line.indexOf(':') + 1).trim();
                    double n = Double.parseDouble(v.replaceAll("[A-Za-z]", ""));
                    return (long) (v.endsWith("G") ? n * 1024 * 1024 : v.endsWith("M") ? n * 1024 : n);
                }
            }
            throw new IllegalStateException("vmmap: no physical footprint");
        }
        Process p = new ProcessBuilder("ps", "-o", "rss=", "-p", pid).start();
        return Long.parseLong(new String(p.getInputStream().readAllBytes()).trim());
    }

    private static void say(String what, String value) {
        System.out.println(what + ": " + value);
    }

    private static void expectThrow(String what, Runnable r) {
        try {
            r.run();
            System.out.println(what + ": NO EXCEPTION");
            System.exit(1);
        } catch (RuntimeException e) {
            System.out.println(what + ": " + e.getClass().getSimpleName() + ": " + e.getMessage());
        }
    }
}
