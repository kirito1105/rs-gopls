package api

import "testing"

func TestCounts(t *testing.T) {
	if got := Count(3); got != 13 {
		t.Fatalf("Count(3) = %d", got)
	}
	if got := Capture(2); got != 9 {
		t.Fatalf("Capture(2) = %d", got)
	}
	if got := Inner(3); got != 7 {
		t.Fatalf("Inner(3) = %d", got)
	}
}
