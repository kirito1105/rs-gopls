package client_test

import (
	"testing"

	"example.test/renames/api"
	"example.test/renames/client"
)

func TestExports(t *testing.T) {
	if got := client.Summary(api.Item{Label: 2}); got != 7 {
		t.Fatalf("Summary = %d", got)
	}
	if got := client.Clicks(api.Both{}, api.Both{}, &api.Both{}); got != 3 {
		t.Fatalf("Clicks = %d", got)
	}
}
