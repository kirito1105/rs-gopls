//go:build editor_recovery

package editor

// These source errors are intentional. Build tags keep the ordinary module build usable.
var OutsideIota = iota

const CycleA = CycleB
const CycleB = CycleA

type TooShort struct{ One int }

func malformedInlay() {
	_ = TooShort{1, 2}
}

func firstMissing() { _ = unresolved }

func recoveredEOF() {
	_ = unresolved