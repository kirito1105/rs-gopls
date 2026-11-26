package importsedit

/*
#include <stdint.h>
static int32_t completion_fixture(void) { return 1; }
*/
import "C" // Keep the cgo declaration separate.

func cgoImport() {
	_ = C.completion_fixture()
	_ = []byte(nil)
}
