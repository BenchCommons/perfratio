// Persistent mode: a function that is slow in its first calls and fast afterwards.
//
// Run: perfratio --persistent -w 0 -r 40 ./persistent_example
// Without perfratio the function runs once.
package main

import (
	"time"

	pr "github.com/BenchCommons/perfratio/sdk/go"
)

func main() {
	calls := 0
	pr.Serve(func() {
		calls++
		// a cold start that settles after 5 calls, like a JIT-compiled function
		if calls <= 5 {
			time.Sleep(40 * time.Millisecond)
		} else {
			time.Sleep(10 * time.Millisecond)
		}
	})
}
