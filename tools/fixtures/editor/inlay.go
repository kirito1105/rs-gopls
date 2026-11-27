package editor

type HintReceiver struct{}

func (HintReceiver) method(amount int) {}

type HintEmbedded struct{ Marker int }

type Payload struct{ Marker int }

type HintRecord struct {
	HintEmbedded
	Count, Capacity int
	Payload         Payload
	Text            string
}

func grouped(first, second int, name string)               {}
func unnamed(int, string)                                  {}
func ignored(_ int, kept int)                              {}
func variadic(prefix string, values ...int)                {}
func unnamedVariadic(int, ...int)                          {}
func predeclaredArgs(pointer *int, enabled, fallback bool) {}
func afterString(name string, amount int)                  {}

func inlayCalls(x int, receiver HintReceiver) {
	grouped(1+2, x+2, "text")
	grouped((x+2)*2, (1+2)*2, "nested")
	unnamed(1, "text")
	ignored(1, 2)
	variadic("prefix", 1, 2)
	unnamedVariadic(0, 1, 2)
	predeclaredArgs(nil, true, false)
	func(amount int) { _ = amount }(2)
	local := func(amount int, names ...string) { _, _ = amount, names }
	local(2, "first", "second")
	receiver.method(2)
	methodValue := receiver.method
	methodValue(3)
	HintReceiver.method(receiver, 4)
	_ = len("text")
	_ = append([]int{}, 1, 2)
	_ = make([]int, 2)
	_ = new(int)
	len := func(size int) int { return size }
	_ = len(2)
	append := func(element int) int { return element }
	_ = append(3)
	boundaryCall(7)
	afterString("中文🙂", 2)
}

func inlayFields(x int) {
	embeddedValue := HintEmbedded{Marker: 1}
	payloadValue := Payload{Marker: 2}
	_ = HintRecord{HintEmbedded{}, x, 4, Payload{Marker: 3}, "same type name"}
	_ = HintRecord{embeddedValue, x, 4, payloadValue, "ordinary identifiers"}
	_ = HintRecord{Count: x, Capacity: 4, Payload: payloadValue}
}

func namedHints(x int) (number int, _ int, text string) {
	if x == 0 {
		return
	}
	if x == 1 {
		return 1 + 2, 4, "literal"
	}
	return x + 2, 5, "variable"
}

const (
	First = iota * 2
	Inherited
	PairA, PairB = iota, 1 << iota
	InheritedA, InheritedB
)

const A = 1
const H = 0x10
const B = 1 + 2
const TextLength = len("中文🙂")
const Reference = H + B
const PlainString = "text"
const ControlString = "text" + "\n"
