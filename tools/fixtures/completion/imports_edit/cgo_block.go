package importsedit

/*
static int completion_fixture_with_block(void) { return 2; }
*/
import "C"

import (
	"fmt"
)

func cgoImportWithBlock() {
	_ = fmt.Sprint(C.completion_fixture_with_block())
	_ = []byte(nil)
}
