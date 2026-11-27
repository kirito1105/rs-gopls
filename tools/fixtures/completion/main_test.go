package completion

import "testing"

func InternalTestOnly() int { return ProductionOnly() }

func TestInternal(t *testing.T) {
	_ = SiblingPublic
	_ = siblingPrivate
	_ = InternalTestOnly
}
