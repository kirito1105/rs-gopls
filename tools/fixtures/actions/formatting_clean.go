package actions

type FormattedEntry struct {
	Name string
	ID   int
}

func NewFormattedEntry() FormattedEntry {
	return FormattedEntry{
		Name: "sample",
		ID:   1,
	}
}
