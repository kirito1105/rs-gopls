package implementationnav

type Value int
type AliasValue = Value
type OtherValue int

type Reader interface {
	ReadPhaseFour(Value) Value
}

type ReaderAlias = Reader

type ReadWriter interface {
	Reader
	WritePhaseFour(Value)
}

type ValueReader struct{}

func (ValueReader) ReadPhaseFour(input Value) Value { return input }

type PointerReader struct{}

func (*PointerReader) ReadPhaseFour(input Value) Value { return input }

type AliasReader struct{}

func (AliasReader) ReadPhaseFour(input AliasValue) AliasValue { return input }

type OtherReader struct{}

func (OtherReader) ReadPhaseFour(input OtherValue) OtherValue { return input }

type EmbeddedValue struct{ ValueReader }
type EmbeddedPointer struct{ *PointerReader }
type EmbeddedPointerValue struct{ PointerReader }
type Ambiguous struct {
	ValueReader
	AliasReader
}

type EmbeddedContract struct{ Reader }

func (EmbeddedContract) WritePhaseFour(Value) {}

type Secret interface {
	secretPhaseFour()
}

type LocalSecret struct{}

func (LocalSecret) secretPhaseFour() {}

type Convert[T any] interface {
	ConvertPhaseFour(T) T
}

type GenericConverter[T any] struct{}

func (GenericConverter[T]) ConvertPhaseFour(input T) T { return input }

type IntConverter struct{}

func (IntConverter) ConvertPhaseFour(input int) int { return input }

type StringConverter struct{}

func (StringConverter) ConvertPhaseFour(input string) string { return input }

type NumericConvert[T ~int | ~int64] interface {
	~int | ~int64
	ConvertPhaseFour(T) T
}

type Empty interface{}

type Never interface {
	NeverPhaseFour()
}

func exercise(reader Reader, alias ReaderAlias, converter Convert[int]) {
	reader.ReadPhaseFour(0)
	alias.ReadPhaseFour(0)
	converter.ConvertPhaseFour(0)
}
