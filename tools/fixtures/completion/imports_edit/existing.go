package importsedit

import (
	j "encoding/json"
	"fmt"
)

func existingImport() {
	_ = fmt.Sprint(j.Valid(nil))
	_ = []byte(nil)
}
