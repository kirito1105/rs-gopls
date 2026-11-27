package semantic_errors

func Add(left, right int) int {
	return left + right
}

func BadCall() int {
	return Add(1)
}

func BadReturn() (int, error) {
	return 1
}

func CallNonFunction(value int) {
	value()
}
