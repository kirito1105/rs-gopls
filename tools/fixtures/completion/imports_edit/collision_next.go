package importsedit

import (
	conflict2 "bytes"
	conflict "strings"
)

func twoCollisions() {
	_ = conflict.TrimSpace
	_ = conflict2.TrimSpace
	_ = []byte(nil)
}
