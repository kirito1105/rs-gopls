package semantic_errors

func UnresolvedValue() int {
	return missingValue
}

var UnresolvedType MissingType

type Record struct {
	Number int
}

func ReadField(record Record) int {
	return record.MissingField
}

func CallMethod(record Record) {
	record.MissingMethod()
}
