import perfratio.PerfratioRegion

/*
 * Persistent mode: a function that is slow in its first calls and fast afterwards.
 *
 * Build: kotlinc PerfratioRegion.kt PersistentExample.kt -include-runtime -d persistent.jar
 * Run:   perfratio --persistent -w 0 -r 40 java -jar persistent.jar
 * Without perfratio the function runs once.
 */
fun main() {
    var calls = 0
    PerfratioRegion.serve {
        calls++
        // a cold start that settles after 5 calls, like a JIT-compiled function
        Thread.sleep(if (calls <= 5) 40L else 10L)
    }
}
