package helper

const PublicConst = 7
const privateConst = 8

var PublicVar = PublicConst
var privateVar = privateConst

type Record struct {
	Public  int
	private int
}

type hidden struct{ field int }

func NewRecord(value int) Record { return Record{Public: value} }

func privateFunc() hidden { return hidden{privateVar} }

// Legacy returns the public constant.
//
// Deprecated: Use PublicConst instead.
func Legacy() int { return PublicConst }

type Box[T any] struct{ Value T }

func (b Box[T]) Get() T       { return b.Value }
func (b *Box[T]) Set(value T) { b.Value = value }

func Map[T, U any](items []T, transform func(T) U) []U {
	out := make([]U, len(items))
	for i, item := range items {
		out[i] = transform(item)
	}
	return out
}
