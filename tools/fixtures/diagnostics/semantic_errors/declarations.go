package semantic_errors

const Repeated = 1

var Repeated = 2

func UnusedVariable() {
	unused := 1
}

func NoNewVariables(value int) int {
	value := value + 1
	return value
}

const Fixed = 1

func ReassignConstant() {
	Fixed = 2
}
