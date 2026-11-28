/*
 * perfratio region client for Kotlin/JVM (protocol 1, SDK 1.0.0).
 * License: MIT OR Apache-2.0. One file, standard library only.
 *
 *     PerfratioRegion.region("sort") { sortTheData() }   // stops also when the block throws
 *     // or: PerfratioRegion.start("sort"); ...; PerfratioRegion.stop("sort")
 *
 * Run the program with `perfratio --region=sort java -jar program.jar`. Without perfratio
 * (PERFRATIO_REGION_FILE unset or empty) every call returns at once. Events are buffered in
 * memory and written by a shutdown hook (exitProcess, return from main and an uncaught
 * exception are covered) or on flush(). Runtime.halt and fatal signals skip it: call flush()
 * first when that matters.
 *
 * Persistent mode (`perfratio --persistent java -jar program.jar`): serve { work } runs the
 * block once per iteration perfratio requests and reports the time of every call. Without
 * perfratio the block runs once. Protocol: docs/specs/persistent-mode.md.
 *
 * Thread-safe; protocol 1 allows one thread per region name. Names are 1-64 ASCII letters,
 * digits or `_`; invalid names are ignored with one warning on stderr. Wire format:
 * docs/specs/region-protocol.md.
 */
package perfratio

import java.io.InputStream
import java.io.OutputStream
import java.nio.charset.StandardCharsets
import java.nio.file.Files
import java.nio.file.Paths
import java.nio.file.StandardOpenOption

object PerfratioRegion {
    const val PROTOCOL = 1
    const val SDK_VERSION = "1.0.0"
    const val SDK_ID = "kotlin/$SDK_VERSION"
    const val DEFAULT_REGION = "default"

    private val NAME = Regex("[A-Za-z0-9_]{1,64}")
    private val path: String? = System.getenv("PERFRATIO_REGION_FILE")?.takeIf { it.isNotEmpty() }
    private val live = System.getenv("PERFRATIO_REGION_LIVE") == "1"
    private val lock = Any()
    private val events = ArrayList<String>()
    private var helloWritten = false
    private var warned = false
    private var hookInstalled = false

    /** True when perfratio asked for region events. */
    fun enabled(): Boolean = path != null

    /** Open the region [name]. */
    fun start(name: String = DEFAULT_REGION) = record("start", name)

    /** Close the region [name]. */
    fun stop(name: String = DEFAULT_REGION) = record("stop", name)

    /** Run [block] inside the region [name]; the region stops also when [block] throws. */
    inline fun <T> region(name: String = DEFAULT_REGION, block: () -> T): T {
        start(name)
        try {
            return block()
        } finally {
            stop(name)
        }
    }

    private fun record(kind: String, name: String) {
        if (path == null) return
        // A stop reads the clock first and a start last, so the bookkeeping stays outside the region.
        var t = if (kind == "stop") System.nanoTime() else 0L
        if (!NAME.matches(name)) {
            val first = synchronized(lock) { !warned.also { warned = true } }
            if (first) {
                System.err.println("perfratio_region: ignoring invalid region name \"$name\" (use 1-64 letters, digits or '_')")
            }
            return
        }
        synchronized(lock) {
            if (!hookInstalled) {
                hookInstalled = true
                Runtime.getRuntime().addShutdownHook(Thread { flush() })
            }
            if (kind == "start") t = System.nanoTime()
            events.add("$kind $name $t\n")
        }
        if (live) flush()
    }

    /** Write the buffered events now. Done automatically at exit. */
    fun flush() {
        val file = path ?: return
        synchronized(lock) {
            if (helloWritten && events.isEmpty()) return
            val text = StringBuilder()
            if (!helloWritten) text.append("hello $PROTOCOL $SDK_ID\n")
            events.forEach { text.append(it) }
            try {
                Files.write(
                    Paths.get(file), text.toString().toByteArray(StandardCharsets.UTF_8),
                    StandardOpenOption.CREATE, StandardOpenOption.APPEND,
                )
            } catch (e: Exception) {
                return // never break the program
            }
            helloWritten = true
            events.clear()
        }
    }

    /**
     * Persistent mode: run [work] for every `run <n>` perfratio sends on stdin and answer on
     * stdout. Returns on `quit` or at the end of input. An exception in [work] is reported as a
     * failed iteration and the loop goes on. Without `--persistent`
     * (PERFRATIO_PERSISTENT_PROTOCOL unset) [work] runs once.
     */
    fun serve(input: InputStream = System.`in`, output: OutputStream = System.out, work: () -> Unit) {
        if (System.getenv("PERFRATIO_PERSISTENT_PROTOCOL") == null && input === System.`in`) {
            work()
            return
        }
        val answer = output.bufferedWriter(StandardCharsets.UTF_8)
        answer.write("@perfratio hello $PROTOCOL $SDK_ID\n")
        answer.flush()
        val requests = input.bufferedReader(StandardCharsets.UTF_8)
        while (true) {
            val line = requests.readLine() ?: break
            val words = line.trim().split(Regex("\\s+"))
            if (words.size == 1 && words[0] == "quit") break
            if (words.size != 2 || words[0] != "run") continue
            var failure: Throwable? = null
            val t0 = System.nanoTime()
            try {
                work()
            } catch (error: Throwable) {
                failure = error
            }
            val t1 = System.nanoTime()
            if (failure != null) {
                val text = "${failure.javaClass.simpleName}: ${failure.message}".replace(Regex("\\s+"), " ")
                answer.write("@perfratio fail ${words[1]} $text\n")
            } else {
                answer.write("@perfratio start iteration $t0\n@perfratio stop iteration $t1\n@perfratio done ${words[1]}\n")
            }
            answer.flush()
        }
    }
}
