package typetest

type Value struct{ Field int }
type Alias = Value
type PointerAlias = *Value
type NamedSlice []Value
type NamedMap map[string]Value
type NamedChan chan Value
type Box[T any] struct{ Item T }
type LocalInterface interface{ Read() *Value }

func Identity[T any](value T) T { return value }
func Factory() func() *Value { return func() *Value { return nil } }
func Results() (*Value, error) { return nil, nil }
func IntResult() int { return 1 }

func Probe(a Alias, p PointerAlias, ns NamedSlice, nm NamedMap, nc NamedChan, box Box[Value], iface LocalInterface) {
    _ = a // alias
    _ = p // pointer-alias
    _ = ns // named-slice
    _ = nm // named-map
    _ = nc // named-channel
    _ = box // named-generic
    _ = iface // named-interface
    _ = box.Item // generic-field
    item := box.Item
    _ = item // inferred-generic-field
    val := Identity(Value{})
    _ = val // generic-result
    val2 := Identity[Value](Value{})
    _ = val2 // explicit-generic-result
    plain := []Value{}
    _ = plain // plain-slice
    ptrs := []*Value{}
    _ = ptrs // pointer-slice
    namedMap := map[string]Value{}
    _ = namedMap // anonymous-map
    ch := make(chan Value)
    _ = ch // builtin-make
    s := "naive"
    _ = s // inferred-string
    i := 1
    _ = i // inferred-int
    b := true
    _ = b // inferred-bool
    var in struct{ Inner int }
    _ = in // anonymous-struct
    type Inner struct{ Field int }
    var inner Inner
    _ = inner // local-named
    cb := Factory()
    _ = cb // inferred-fn
    _ = Results // tuple-callee
    _ = IntResult // builtin-result-callee
}

func Generic[T any](v T) T {
    tmp := v
    _ = tmp // generic-local-var
    return v
}
