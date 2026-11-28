package perfratioregion

import (
	"bytes"
	"strings"
	"testing"
)

func TestValidName(t *testing.T) {
	for name, want := range map[string]bool{"sort": true, "a_1": true, "": false, "a-b": false, strings.Repeat("a", 65): false, strings.Repeat("a", 64): true} {
		if validName(name) != want {
			t.Errorf("validName(%q) != %v", name, want)
		}
	}
}

func TestServeIO(t *testing.T) {
	var out bytes.Buffer
	calls := 0
	ServeIO(func() {
		calls++
		if calls == 2 {
			panic("bad\nthing")
		}
	}, strings.NewReader("run 0\nrun 1\nnoise\nrun 2\nquit\nrun 3\n"), &out)
	lines := strings.Split(strings.TrimSpace(out.String()), "\n")
	if lines[0] != "@perfratio hello 1 "+SDKID {
		t.Fatalf("hello: %q", lines[0])
	}
	if calls != 3 || lines[4] != "@perfratio fail 1 panic: bad thing" || lines[len(lines)-1] != "@perfratio done 2" {
		t.Fatalf("calls %d, output:\n%s", calls, out.String())
	}
}
