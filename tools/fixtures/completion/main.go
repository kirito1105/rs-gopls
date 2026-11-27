package completion

import (
	"example.test/completion/pkg/embedded"
	"example.test/completion/pkg/helper"
)

type S struct {
	A, Field int
}

type ScopeBox[TypeParam any] struct {
	Value TypeParam
}

func (receiver ScopeBox[TypeParam]) Scope(parameter TypeParam) (result TypeParam) {
	localValue := parameter
	_ = receiver.Value
	_ = localValue
	return result
}

func selectors(foo embedded.Outer, pointer *embedded.Outer, double **embedded.Outer, iface embedded.Interface, interfacePointer *embedded.Interface) {
	_ = foo.Basic
	_ = foo.LeftOnly
	_ = foo.RightOnly
	_ = foo.LeftMethod()
	_ = pointer.Basic
	_ = (*double).Basic
	_ = iface.Invoke()
	_ = (*interfacePointer).Invoke()
	_ = embedded.Outer.Direct
	_ = (*embedded.Outer).PointerMethod
	_ = embedded.NewOuter().Direct()
	_ = useInt(foo.Basic)
	_ = foo.Basic
	var withInterface embedded.WithInterface
	_ = withInterface.Invoke()
}

func useInt(value int) int { return value }

func parameterTarget(a int) int { return a }

func literals() {
	localValue := 2
	_ = S{}
	_ = S{A: 1, Field: localValue}
	_ = S{1, 2}
	_ = embedded.Outer{Left: embedded.Left{LeftOnly: 1}}
	_ = embedded.Outer{Basic: 1, Left: embedded.Left{}}
	_ = helper.Record{Public: 1}
}

func types(arg helper.Record) {
	var value helper.Record
	var instantiated helper.Box[string]
	_ = arg
	_ = value
	_ = instantiated.Value
	_ = instantiated.Get()
	_ = helper.Map([]int{1}, useInt)
}

func (s S) ReceiverTarget() {}

func labels(input any) {
OuterLoop:
	for range []int{1} {
		switch input.(type) {
		case int:
			break OuterLoop
		default:
			continue OuterLoop
		}
	}
	goto ExitPoint
ExitPoint:
	_ = func() {
		goto InnerPoint
	InnerPoint:
	}
}

const sequence = iota

var outsideConst = sequence

func suppression() {
	// normal comment Moon
	_ = "normal string Moon"
	_ = 123
}
