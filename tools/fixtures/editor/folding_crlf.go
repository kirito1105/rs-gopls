package editor

import (
	"fmt"
	"strconv"
)

// 中文🙂 first line comment.
// Second consecutive comment line.

// An empty line separates this group.
// The group continues here.

/*
多行块注释🙂
Second block-comment line.
*/

//region Consecutive comments share this region's boundaries
// A comment on the next line keeps the whole group contiguous.
//endregion

// region Outer region
var outerFold = 1

// region Inner region
var innerFold = 2

// endregion
// endregion

// <editor-fold desc="Outer editor fold">
var outerEditorFold = 1

// <editor-fold desc="Inner editor fold">
var innerEditorFold = 2

// </editor-fold>
// </editor-fold>

var (
	groupedVarA = 1
	groupedVarB = 2
)

var (
	singleVar = []int{
		1,
		2,
	}
)

const (
	groupedConstA = 1
	groupedConstB = 2
)

const (
	singleConst = (1 +
		2)
)

type (
	groupedTypeA int
	groupedTypeB string
)

type (
	singleType struct {
		Field string
	}
)

type foldInterface interface {
	Call(amount int) int
}

type foldStruct struct {
	Nested struct {
		Flag bool
	}
}

func foldBlocks(ch chan int, value any) {
	if x := 1; x > 0 {
		_ = x
	} else {
		_ = ch
	}
	for idx := range 2 {
		_ = idx
	}
	{
		_ = value
	}
	nested := func(amount int) int {
		return amount
	}
	switch value {
	case 0:
	case 1:
		_ = nested(2)
	default:
		_ = value
	}
	switch value.(type) {
	case string:
		_ = value
	default:
		_ = value
	}
	select {
	case <-ch:
		_ = value
	default:
	}
	_ = []int{
		1,
		2,
	}
	_ = []int{}
	_ = `第一行🙂
第二行
`
	fmt.Println("中文🙂", strconv.Itoa(nested(2)))
}
