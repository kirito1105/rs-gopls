package worker

type Reader interface {
	Read() int
}

type ReadCloser interface {
	Reader
	Close()
}

type Service interface {
	ReadCloser
	Reset()
}

type Resetter interface {
	Reset()
}

type Base struct {
	Count int
}

func (base Base) Read() int {
	return base.Count
}

type Derived struct {
	Base
}

func (Derived) Close() {}

func (Derived) Reset() {}

type Alias = Derived

var _ Service = Derived{}
var _ Resetter = Derived{}

func Leaf(value int) int {
	return value + 1
}

func Recursive(depth int) int {
	if depth <= 0 {
		return Leaf(depth)
	}
	return Leaf(depth) + Recursive(depth-1)
}

func (derived Derived) Run() int {
	return Leaf(derived.Read())
}
