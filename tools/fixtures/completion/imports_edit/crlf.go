package importsedit

import "fmt"

func crlfImport() {
	_ = fmt.Sprint("😀中文", /* 字段 😀 */ []byte(nil))
}
