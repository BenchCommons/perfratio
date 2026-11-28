// Package perfratioregion is the perfratio region client for Go (protocol 1, SDK 1.0.0).
//
// License: MIT OR Apache-2.0. One file, standard library only.
//
//	defer perfratioregion.Flush()               // first line of main
//	stop := perfratioregion.Region("sort")      // or Start("sort") ... Stop("sort")
//	sortTheData()
//	stop()                                      // `defer perfratioregion.Region("sort")()` also works
//
// Run the program with `perfratio --region=sort ./program`. Without perfratio
// (PERFRATIO_REGION_FILE unset or empty) every call returns at once. Events are buffered in
// memory and written by Flush. Go has no exit handlers: defer Flush in main (it also runs
// while a panic unwinds) and call Flush before os.Exit, which skips deferred calls.
//
// Persistent mode (`perfratio --persistent ./program`): Serve calls a function once per
// iteration perfratio requests and reports the time of every call. Without perfratio the
// function runs once. Protocol: docs/specs/persistent-mode.md.
//
// Safe for concurrent use; protocol 1 allows one goroutine per region name. Names are 1-64
// ASCII letters, digits or `_`; invalid names are ignored with one warning on stderr. Wire
// format: docs/specs/region-protocol.md.
package perfratioregion

import (
	"bufio"
	"fmt"
	"io"
	"os"
	"strconv"
	"strings"
	"sync"
	"time"
)

const (
	// Protocol is the region protocol version this client speaks.
	Protocol = 1
	// SDKVersion is the version of this client.
	SDKVersion = "1.0.0"
	// SDKID is sent in the hello line.
	SDKID = "go/" + SDKVersion
	// DefaultRegion is the name used when none is given.
	DefaultRegion = "default"
)

var (
	// time.Since uses the monotonic clock reading of epoch
	epoch = time.Now()
	mu    sync.Mutex
	path  = os.Getenv("PERFRATIO_REGION_FILE")
	live  = os.Getenv("PERFRATIO_REGION_LIVE") == "1"

	events       []string
	helloWritten bool
	warned       bool
)

func now() int64 { return int64(time.Since(epoch)) }

func validName(name string) bool {
	if len(name) < 1 || len(name) > 64 {
		return false
	}
	for i := 0; i < len(name); i++ {
		c := name[i]
		if !(c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || c >= '0' && c <= '9' || c == '_') {
			return false
		}
	}
	return true
}

// Enabled reports whether perfratio asked for region events.
func Enabled() bool { return path != "" }

func record(kind, name string) {
	if path == "" {
		return
	}
	// A stop reads the clock first and a start last, so the bookkeeping stays outside the region.
	var t int64
	if kind == "stop" {
		t = now()
	}
	if !validName(name) {
		mu.Lock()
		first := !warned
		warned = true
		mu.Unlock()
		if first {
			fmt.Fprintf(os.Stderr, "perfratio_region: ignoring invalid region name %q (use 1-64 letters, digits or '_')\n", name)
		}
		return
	}
	mu.Lock()
	if kind == "start" {
		t = now()
	}
	events = append(events, kind+" "+name+" "+strconv.FormatInt(t, 10)+"\n")
	mu.Unlock()
	if live {
		Flush()
	}
}

// Start opens the region name.
func Start(name string) { record("start", name) }

// Stop closes the region name.
func Stop(name string) { record("stop", name) }

// Region opens the region name and returns the function that closes it, for `defer`:
// `defer perfratioregion.Region("sort")()`.
func Region(name string) func() {
	Start(name)
	return func() { Stop(name) }
}

// Flush writes the buffered events now. Call it with defer in main and before os.Exit.
func Flush() {
	if path == "" {
		return
	}
	mu.Lock()
	defer mu.Unlock()
	if helloWritten && len(events) == 0 {
		return
	}
	var text strings.Builder
	if !helloWritten {
		fmt.Fprintf(&text, "hello %d %s\n", Protocol, SDKID)
	}
	for _, event := range events {
		text.WriteString(event)
	}
	file, err := os.OpenFile(path, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o644)
	if err != nil {
		return // never break the program
	}
	defer file.Close()
	if _, err := file.WriteString(text.String()); err != nil {
		return
	}
	helloWritten = true
	events = nil
}

// Serve is the persistent mode: it calls work for every `run <n>` perfratio sends on stdin
// and answers on stdout. It returns on `quit` or at the end of input. A panic in work is
// reported as a failed iteration and the loop goes on. Without `--persistent`
// (PERFRATIO_PERSISTENT_PROTOCOL unset) work runs once.
func Serve(work func()) {
	if _, ok := os.LookupEnv("PERFRATIO_PERSISTENT_PROTOCOL"); !ok {
		work()
		return
	}
	ServeIO(work, os.Stdin, os.Stdout)
}

// ServeIO is Serve on explicit streams, for tests.
func ServeIO(work func(), in io.Reader, out io.Writer) {
	fmt.Fprintf(out, "@perfratio hello %d %s\n", Protocol, SDKID)
	scanner := bufio.NewScanner(in)
	for scanner.Scan() {
		words := strings.Fields(scanner.Text())
		if len(words) == 1 && words[0] == "quit" {
			return
		}
		if len(words) != 2 || words[0] != "run" {
			continue
		}
		t0 := now()
		failure := call(work)
		t1 := now()
		if failure != "" {
			fmt.Fprintf(out, "@perfratio fail %s %s\n", words[1], failure)
		} else {
			fmt.Fprintf(out, "@perfratio start iteration %d\n@perfratio stop iteration %d\n@perfratio done %s\n", t0, t1, words[1])
		}
	}
}

// call runs work and returns the text of a panic, or "" when it returned normally.
func call(work func()) (failure string) {
	defer func() {
		if r := recover(); r != nil {
			failure = strings.Join(strings.Fields(fmt.Sprint("panic: ", r)), " ")
			if failure == "" {
				failure = "panic"
			}
		}
	}()
	work()
	return ""
}
