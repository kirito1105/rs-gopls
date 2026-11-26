package clean

import _ "time/tzdata"

type Reader interface {
	Read() string
}

type NamedReader interface {
	Reader
	Name() string
}

type Label struct {
	Text string
}

func (label Label) Name() string {
	return label.Text
}

type Buffer[T ~string] struct {
	Label
	Value T
}

func (buffer Buffer[T]) Read() string {
	return string(buffer.Value)
}

type StringBuffer = Buffer[string]

var _ NamedReader = StringBuffer{}

func Snapshot(reader NamedReader) func() string {
	name := reader.Name()
	return func() string { return name + ": " + reader.Read() }
}
