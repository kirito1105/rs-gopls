package api

type Item struct {
	Label int `json:"Label"`
}

const Bonus = 5

// Count sums the local value and the map entry addressed by that value.
func Count(seed int) int {
	value := seed
	value, extra := value+1, 1
	byKey := map[int]int{value: 8}
	add := func() int { return value + byKey[value] }
	return add() + extra
}

var total = 7

func Capture(input int) int {
	result := input
	return result + total
}

func Inner(seed int) int {
	if active := seed + 1; active > 0 {
		return seed + active
	}
	return seed
}
