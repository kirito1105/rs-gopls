package main

import (
	"example.com/hoversig/helper"
)

// UnaryOp transforms a single integer.
type UnaryOp func(x int) int

// Add sums two integers with a label.
func Add(a, b int, label string) (int, error) {
	return a + b, nil
}

func inner(n int) int {
	return n + 1
}

func takeItem(item helper.Item) string {
	return item.Name
}

func exercise(f helper.Formatter, cb func(count int, tag string) bool, op UnaryOp) {
	sum, err := Add(inner(1), 2, "ok")
	_ = sum
	_ = err

	joined := helper.Join("-", "a", "b", "c")
	_ = joined

	mapped := helper.Map([]int{1, 2}, func(v int) string {
		return helper.DefaultName
	})
	_ = mapped

	var box helper.IntBox
	box.Set(42)
	(*helper.Box[int]).Set(&box, 99)

	_ = f.Format("pre", 1, 2)
	_ = cb(3, "tag")
	_ = op(7)

	item := helper.Item{Name: "alpha"}
	_ = item.ID
	_ = item.Name
	_ = takeItem(helper.Item{Name: "beta"})

	nums := make([]int, 0, 4)
	nums = append(nums, 1, 2, 3)
	_ = len(nums)
	_ = helper.StatusOK
	_ = helper.StatusError
}

func main() {}

