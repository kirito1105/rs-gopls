package hierarchy_test

import (
	"testing"

	"example.test/hierarchy"
	"example.test/hierarchy/worker"
)

func TestDispatch(t *testing.T) {
	value := worker.Derived{Base: worker.Base{Count: 3}}
	if got, want := hierarchy.Dispatch(value, value), 15; got != want {
		t.Fatalf("Dispatch = %d, want %d", got, want)
	}
}

func TestLeaf(t *testing.T) {
	if got, want := worker.Leaf(2), 3; got != want {
		t.Fatalf("Leaf = %d, want %d", got, want)
	}
}

func TestRecurse(t *testing.T) {
	if got, want := hierarchy.Recurse(2), 6; got != want {
		t.Fatalf("Recurse = %d, want %d", got, want)
	}
}
