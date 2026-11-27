package actions

/*
#include <stdlib.h>
*/
import "C"

import (
	"fmt"
	"unsafe"
)

func Release(value string) {
	cString := C.CString(value)
	C.free(unsafe.Pointer(cString))
}
