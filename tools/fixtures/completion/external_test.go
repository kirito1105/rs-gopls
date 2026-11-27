package completion_test

import (
	"testing"

	production "example.test/completion"
)

func ExternalTestOnly() int { return production.ProductionOnly() }

func TestExternal(t *testing.T) {
	_ = production.SiblingPublic
	_ = production.ProductionOnly()
	_ = ExternalTestOnly
}
