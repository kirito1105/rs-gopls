package editor

import (
	. "math"
	"strings"
	clock "time"
)

const (
	PackageConstant = iota
	PackageInherited
)

var PackageVariable = Record{F: PackageConstant}

type Integer int

type Embedded struct {
	Value int
}

type Record struct {
	Embedded
	F    int
	Text string
}

type Alias = Record

type Port interface {
	Read(value int) (result int)
}

type Bag[T any] struct {
	Items []T
}

func Top[T ~int](parameter T) (named T) {
	return parameter
}

func (receiver Record) Read(parameter int) (named int) {
	named = receiver.F + parameter
	return
}

func symbols() {
	_ = clock.Second
	_ = strings.ToUpper("扩展🙂")
	_ = Pi
	_ = Alias{F: 1}
	_ = Record{Embedded: Embedded{Value: 2}, F: 3}
	var port Port = Record{}
	_ = Bag[Integer]{Items: []Integer{1}}
	_ = Top[Integer](1)
	_ = port.Read(1)
	_ = Record{}.Read(2)
	_ = boundaryValue
	_ = boundaryCall(1)
	_ = PackageVariable.F
	{
		clock := 2
		_ = clock
	}
}

func predeclared() {
	var values []string
	var number int
	_ = len(values)
	_ = append(values, "x")
	var pointer *int = nil
	_, _, _, _ = true, false, pointer, number
	const LocalIota = iota
	{
		type int string
		type string bool
		len := func(count int) int { return count }
		append := func(count string) string { return count }
		true, false := 1, 2
		nil := 3
		const iota = 4
		const ShadowedIota = iota
		_, _, _, _, _, _ = len("text"), append(true == false), true, false, nil, ShadowedIota
	}
}

func takesPointer(value *int) { _ = value }

func writes(obj Record, ch chan int, xs []int, a []int, i int) {
	// Cursor on x in this comment suppresses usage highlighting.
	var x = 1
	const y = 2
	var p = &x
	var k, v int
	x = y
	x += y
	x++
	x--
	ch <- v
	v = <-ch
	(x) = y
	*p = y
	takesPointer(&x)
	obj.F = y
	a[i] = y
	_ = Record{F: v}
	_ = map[int]int{k: v}
	for k, v = range xs {
		_ = k + v
	}
	x, next := x+y, 0
	{
		x := next
		_ = x
	}
	_ = x
}

func exits(x int) int {
	if x == 0 {
		return 0
	}
	if x == 1 {
		panic("outer")
	}
	if x == 2 {
		defer panic("deferred")
	}
	if x == 3 {
		go panic("goroutine")
	}
	inner := func() int {
		if x < 0 {
			panic("inner")
		}
		return x
	}
	{
		panic := func(message string) { _ = message }
		panic("shadowed")
	}
	return inner()
}

func loops(ch chan int, value any) {
Outer:
	for {
		if value == nil {
			break Outer
		}
		for {
			if value == 10 {
				continue Outer
			}
			if value == 11 {
				continue
			}
			break
		}
		if value == 12 {
			continue
		}
		if value == 13 {
			break
		}
		switch value {
		case 0:
			break
		default:
			_ = value
		}
		switch value.(type) {
		case string:
			break
		default:
			_ = value
		}
		select {
		case <-ch:
			break
		default:
			_ = value
		}
	}
}

func namedSlots(x int) (left int, right string) {
	left = x
	if x == 0 {
		return
	}
	if x == 1 {
		return 1 + 2, "named"
	}
	if x == 2 {
		return slotSource()
	}
	return x + 2, "fallback"
}

func slotSource() (int, string) { return 7, "forward" }

func anonymousSlots(flag bool) (int, string) {
	if flag {
		return slotSource()
	}
	return 23, "anonymous"
}
