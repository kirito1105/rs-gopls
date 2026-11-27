package actions

import "strings"

func MissingStandardImport() string {
	return fmt.Sprint(42)
}

func MissingLocalImport() string {
	return helper.Label()
}

func UnusedLocal() {
	unused := 42
}

func Reassign(value int) int {
	value := value + 1
	return value
}
