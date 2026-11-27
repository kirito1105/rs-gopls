package api_test

import (
	"testing"

	"example.com/implementation-navigation/api"
	"example.com/implementation-navigation/email"
	"example.com/implementation-navigation/sms"
)

func TestNotifierAcrossPackages(t *testing.T) {
	tests := []struct {
		name     string
		notifier api.Notifier
		want     string
	}{
		{"email", email.Sender{}, "email: hello"},
		{"sms", &sms.Sender{}, "sms: hello"},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			if got := tc.notifier.Notify("hello"); got != tc.want {
				t.Errorf("Notify(%q) = %q, want %q", "hello", got, tc.want)
			}
		})
	}
}
