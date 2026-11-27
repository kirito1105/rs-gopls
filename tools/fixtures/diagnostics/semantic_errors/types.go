package semantic_errors

var WrongValue int = "text"

type Writer interface {
	Write()
}

type Item struct {
	Number int
}

var _ Writer = Item{}

var UnknownField = Item{Missing: 1}

func DuplicateCase(value int) int {
	switch value {
	case 1:
		return 1
	case 1:
		return 2
	default:
		return 0
	}
}

var BlankValue = _
