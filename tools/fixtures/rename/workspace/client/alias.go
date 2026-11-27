package client

import helper "example.test/renames/api"

func Alias() int {
	return helper.Item{Label: helper.Bonus}.Label
}
