// Three phases of sleep; only the middle one is the region "middle".
//
// Run: perfratio --region=middle ./example   (expected region time: 0.3 s)
// Modes: example [panic]   "panic" panics inside the region (the region stops while the
// panic unwinds and the deferred Flush writes the events).
package main

import (
	"os"
	"time"

	pr "github.com/BenchCommons/perfratio/sdk/go"
)

func main() {
	defer pr.Flush()
	time.Sleep(100 * time.Millisecond)
	func() {
		defer pr.Region("middle")()
		time.Sleep(300 * time.Millisecond)
		if len(os.Args) > 1 && os.Args[1] == "panic" {
			panic("boom")
		}
	}()
	time.Sleep(100 * time.Millisecond)
}
