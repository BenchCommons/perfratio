import perfratio.PerfratioRegion;

/**
 * Three phases of sleep; only the middle one is the region "middle".
 *
 * Build: javac -d out perfratio/PerfratioRegion.java Example.java
 * Run:   perfratio --region=middle java -cp out Example   (expected region time: 0.3 s)
 * Modes: Example [exit|throw]   "exit" calls System.exit(3) and "throw" throws, both inside the
 * region (flush on exit and on error).
 */
public final class Example {
    public static void main(String[] args) throws Exception {
        String mode = args.length > 0 ? args[0] : "";
        Thread.sleep(100);
        try (PerfratioRegion.Scope scope = PerfratioRegion.region("middle")) {
            Thread.sleep(300);
            if (mode.equals("exit")) {
                System.exit(3);
            }
            if (mode.equals("throw")) {
                throw new IllegalStateException("boom");
            }
        }
        Thread.sleep(100);
    }
}
