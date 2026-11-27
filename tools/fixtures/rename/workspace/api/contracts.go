package api

type First interface {
	Click() int
	One()
}

type Second interface {
	Click() int
	Two()
}

type Extended interface {
	First
	Extra()
}

type Both struct{}

func (Both) Click() int { return 1 }
func (Both) One()       {}
func (Both) Two()       {}

type Pointer struct{}

func (*Pointer) Click() int { return 2 }
func (*Pointer) One()       {}

type Wrapped struct{ Both }

func (Wrapped) Extra() {}

type Different struct{}

func (Different) Click(x int) int { return x }
