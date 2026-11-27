package clean_test

import (
	"testing"

	"example.test/diagnostics/clean"
)

func TestSnapshot(t *testing.T) {
	buffer := clean.StringBuffer{Label: clean.Label{Text: "buffer"}, Value: "ready"}
	read := clean.Snapshot(buffer)
	if got, want := read(), "buffer: ready"; got != want {
		t.Fatalf("snapshot = %q, want %q", got, want)
	}
}
