/*
 * perfratio region client for Java (protocol 1, SDK 1.0.0). Java 8 or later.
 * License: MIT OR Apache-2.0. One file, no dependencies. Kotlin and Scala can use it as is
 * (sdk/kotlin has an idiomatic Kotlin version).
 *
 *     try (PerfratioRegion.Scope scope = PerfratioRegion.region("sort")) {
 *         sortTheData();                       // stops also when this throws
 *     }
 *     // or: PerfratioRegion.start("sort"); ...; PerfratioRegion.stop("sort");
 *
 * Run the program with `perfratio --region=sort java Program`. Without perfratio
 * (PERFRATIO_REGION_FILE unset or empty) every call returns at once. Events are buffered in
 * memory and written by a shutdown hook (System.exit, return from main and an uncaught
 * exception are covered) or on flush(). Runtime.halt and fatal signals skip it: call flush()
 * first when that matters.
 *
 * Persistent mode (`perfratio --persistent java Program`): serve(Runnable) runs the work once
 * per iteration perfratio requests and reports the time of every call, so JIT warm-up shows
 * in the export. Without perfratio the work runs once. Protocol: docs/specs/persistent-mode.md.
 *
 * Thread-safe; protocol 1 allows one thread per region name. Names are 1-64 ASCII letters,
 * digits or `_`; invalid names are ignored with one warning on stderr. Wire format:
 * docs/specs/region-protocol.md.
 */
package perfratio;

import java.io.BufferedReader;
import java.io.IOException;
import java.io.InputStream;
import java.io.InputStreamReader;
import java.io.OutputStream;
import java.io.PrintStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Paths;
import java.nio.file.StandardOpenOption;
import java.util.ArrayList;
import java.util.List;
import java.util.regex.Pattern;

public final class PerfratioRegion {
    public static final int PROTOCOL = 1;
    public static final String SDK_VERSION = "1.0.0";
    public static final String SDK_ID = "java/" + SDK_VERSION;
    public static final String DEFAULT_REGION = "default";

    private static final Pattern NAME = Pattern.compile("[A-Za-z0-9_]{1,64}");
    private static final String PATH = nonEmpty(System.getenv("PERFRATIO_REGION_FILE"));
    private static final boolean LIVE = "1".equals(System.getenv("PERFRATIO_REGION_LIVE"));
    private static final Object LOCK = new Object();
    private static final List<String> EVENTS = new ArrayList<String>();
    private static boolean helloWritten;
    private static boolean warned;
    private static boolean hookInstalled;

    private PerfratioRegion() {}

    private static String nonEmpty(String value) {
        return value == null || value.isEmpty() ? null : value;
    }

    /** True when perfratio asked for region events. */
    public static boolean enabled() {
        return PATH != null;
    }

    /** Open the region {@code name}. */
    public static void start(String name) {
        record("start", name);
    }

    /** Close the region {@code name}. */
    public static void stop(String name) {
        record("stop", name);
    }

    /** A region that closes with try-with-resources, also when the block throws. */
    public static Scope region(String name) {
        start(name);
        return new Scope(name);
    }

    /** Closes its region. */
    public static final class Scope implements AutoCloseable {
        private final String name;

        private Scope(String name) {
            this.name = name;
        }

        @Override
        public void close() {
            stop(name);
        }
    }

    private static void record(String kind, String name) {
        if (PATH == null) {
            return;
        }
        // A stop reads the clock first and a start last, so the bookkeeping stays outside the region.
        long t = kind.equals("stop") ? System.nanoTime() : 0L;
        if (name == null || !NAME.matcher(name).matches()) {
            boolean first;
            synchronized (LOCK) {
                first = !warned;
                warned = true;
            }
            if (first) {
                System.err.println("perfratio_region: ignoring invalid region name \"" + name
                        + "\" (use 1-64 letters, digits or '_')");
            }
            return;
        }
        synchronized (LOCK) {
            if (!hookInstalled) {
                hookInstalled = true;
                Runtime.getRuntime().addShutdownHook(new Thread(new Runnable() {
                    @Override
                    public void run() {
                        flush();
                    }
                }));
            }
            if (kind.equals("start")) {
                t = System.nanoTime();
            }
            EVENTS.add(kind + " " + name + " " + t + "\n");
        }
        if (LIVE) {
            flush();
        }
    }

    /** Write the buffered events now. Done automatically at exit. */
    public static void flush() {
        if (PATH == null) {
            return;
        }
        synchronized (LOCK) {
            if (helloWritten && EVENTS.isEmpty()) {
                return;
            }
            StringBuilder text = new StringBuilder();
            if (!helloWritten) {
                text.append("hello ").append(PROTOCOL).append(' ').append(SDK_ID).append('\n');
            }
            for (String event : EVENTS) {
                text.append(event);
            }
            try {
                Files.write(Paths.get(PATH), text.toString().getBytes(StandardCharsets.UTF_8),
                        StandardOpenOption.CREATE, StandardOpenOption.APPEND);
            } catch (IOException | RuntimeException e) {
                return; // never break the program
            }
            helloWritten = true;
            EVENTS.clear();
        }
    }

    /**
     * Persistent mode: run {@code work} for every {@code run <n>} perfratio sends on stdin and
     * answer on stdout. Returns on {@code quit} or at the end of input. An exception in the work
     * is reported as a failed iteration and the loop goes on. Without {@code --persistent}
     * (PERFRATIO_PERSISTENT_PROTOCOL unset) the work runs once.
     */
    public static void serve(Runnable work) throws IOException {
        if (System.getenv("PERFRATIO_PERSISTENT_PROTOCOL") == null) {
            work.run();
            return;
        }
        serve(work, System.in, System.out);
    }

    /** {@link #serve(Runnable)} on explicit streams, for tests. */
    public static void serve(Runnable work, InputStream in, OutputStream out) throws IOException {
        PrintStream answer = new PrintStream(out, false, "UTF-8");
        answer.print("@perfratio hello " + PROTOCOL + " " + SDK_ID + "\n");
        answer.flush();
        BufferedReader requests = new BufferedReader(new InputStreamReader(in, StandardCharsets.UTF_8));
        String line;
        while ((line = requests.readLine()) != null) {
            String[] words = line.trim().split("\\s+");
            if (words.length == 1 && words[0].equals("quit")) {
                break;
            }
            if (words.length != 2 || !words[0].equals("run")) {
                continue;
            }
            Throwable failure = null;
            long t0 = System.nanoTime();
            try {
                work.run();
            } catch (Throwable error) { // reported to perfratio, the loop goes on
                failure = error;
            }
            long t1 = System.nanoTime();
            if (failure != null) {
                String text = (failure.getClass().getSimpleName() + ": " + failure.getMessage())
                        .replaceAll("\\s+", " ");
                answer.print("@perfratio fail " + words[1] + " " + text + "\n");
            } else {
                answer.print("@perfratio start iteration " + t0 + "\n@perfratio stop iteration " + t1
                        + "\n@perfratio done " + words[1] + "\n");
            }
            answer.flush();
        }
    }
}
