import perfratio.PerfratioRegion
import kotlin.system.exitProcess

/*
 * Three phases of sleep; only the middle one is the region "middle".
 *
 * Build: kotlinc PerfratioRegion.kt Example.kt -include-runtime -d example.jar
 * Run:   perfratio --region=middle java -jar example.jar   (expected region time: 0.3 s)
 * Modes: example.jar [exit|throw]   "exit" calls exitProcess(3) and "throw" throws, both inside
 * the region (flush on exit and on error).
 */
fun main(args: Array<String>) {
    val mode = args.firstOrNull() ?: ""
    Thread.sleep(100)
    PerfratioRegion.region("middle") {
        Thread.sleep(300)
        if (mode == "exit") exitProcess(3)
        if (mode == "throw") throw IllegalStateException("boom")
    }
    Thread.sleep(100)
}
