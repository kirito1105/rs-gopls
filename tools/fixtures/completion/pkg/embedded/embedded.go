package embedded

type Left struct {
	Shared   int
	LeftOnly int
	hidden   int
}

func (Left) Collision() int  { return 1 }
func (Left) LeftMethod() int { return 2 }
func (*Left) LeftPointer()   {}

type Right struct {
	Shared    string
	RightOnly int
}

func (Right) Collision() string { return "right" }
func (*Right) RightPointer()    {}

type Outer struct {
	Left
	*Right
	Basic int
}

func NewOuter() Outer           { return Outer{} }
func (o Outer) Direct() int     { return o.Basic }
func (o *Outer) PointerMethod() {}

type Interface interface{ Invoke() int }

type WithInterface struct{ Interface }
