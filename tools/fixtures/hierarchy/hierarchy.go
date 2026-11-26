package hierarchy

import "example.test/hierarchy/worker"

func Dispatch(service worker.Service, derived worker.Alias) int {
	service.Close()
	return service.Read() + derived.Run() + worker.Leaf(len("🙂")) + worker.Leaf(2)
}

func Recurse(depth int) int {
	return worker.Recursive(depth)
}
