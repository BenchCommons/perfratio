import perfratio.PerfratioRegion;

/**
 * Persistent mode: a function that is slow in its first calls and fast afterwards.
 *
 * Run: perfratio --persistent -w 0 -r 40 java -cp out PersistentExample
 * Without perfratio the function runs once.
 */
public final class PersistentExample {
    private static int calls = 0;

    public static void main(String[] args) throws Exception {
        PerfratioRegion.serve(new Runnable() {
            @Override
            public void run() {
                calls++;
                // a cold start that settles after 5 calls, like a JIT-compiled function
                try {
                    Thread.sleep(calls <= 5 ? 40 : 10);
                } catch (InterruptedException e) {
                    Thread.currentThread().interrupt();
                }
            }
        });
    }
}
