package helper

// Status codes shared across the helper package.
const (
	StatusOK    = 200 // request succeeded
	StatusError = 500
)

// DefaultName is the fallback display name.
var DefaultName = "guest"

// Base holds common identity metadata.
type Base struct {
	// ID uniquely identifies the record.
	ID int64 `json:"id"`
}

// Item represents a stored record.
type Item struct {
	Base
	Name string `json:"name,omitempty"` // display label
}

// Formatter formats values with a prefix.
type Formatter interface {
	// Format renders args using prefix.
	Format(prefix string, args ...any) string
}

// Box wraps a single typed value.
type Box[T any] struct {
	Value T
}

// Set replaces the stored value.
func (b *Box[T]) Set(v T) {
	b.Value = v
}

// IntBox aliases a specialized Box.
type IntBox = Box[int]

//go:noinline
// Map transforms each element using fn.
func Map[T any, U any](items []T, fn func(T) U) []U {
	out := make([]U, len(items))
	for i, item := range items {
		out[i] = fn(item)
	}
	return out
}

// Join concatenates parts separated by sep.
func Join(sep string, parts ...string) string {
	result := ""
	for i, part := range parts {
		if i > 0 {
			result += sep
		}
		result += part
	}
	return result
}
