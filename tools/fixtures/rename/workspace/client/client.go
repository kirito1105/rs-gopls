package client

import "example.test/renames/api"

// Label and Bonus are literal text in this example.
const description = "Label Bonus"

func Summary(item api.Item) int {
	return item.Label + api.Bonus
}

func Clicks(first api.First, second api.Second, both *api.Both) int {
	return first.Click() + second.Click() + both.Click()
}

func Promoted(value api.Extended, wrapped api.Wrapped) int {
	return value.Click() + wrapped.Click()
}
