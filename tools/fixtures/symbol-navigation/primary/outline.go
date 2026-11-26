package symbols

import (
	"example.test/dependency"
	clock "time"
)

const (
	First, Second = iota, iota
	Third, Fourth
)

var (
	Label, Title string
	VendorWitness = dependency.VendorOnlySymbol
)
var Emoji = "🙂"; var AfterEmoji int

type (
	Alias = int
	Count int
	Embedded struct{}
)

type Box[T any] struct{ Value T }

type Record[T any] struct {
	X, Y int
	Embedded
	*clock.Location
	Box[T]
	Inner struct{ NestedField int }
	Port interface{ InlineMethod() }
}

type Base interface{ Begin() }

type Handler interface {
	Base
	HandleRequest(int) error
}

type Constraint interface {
	~int
	Count | ~string
}

type UnionConstraint interface {
	~int | ~string
	Marker()
}

var _ Handler = (*Record[int])(nil)

func MakeHTTPServer() {
	var localOnly int
	_ = localOnly
}

func init() {}
func init() {}
