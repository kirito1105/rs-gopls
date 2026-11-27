package importsedit

// The first block keeps its declaration comment.
import (
	"fmt" // Standard library.
)

// The second block keeps its declaration comment.
import (
	_ "image/png"
	. "math"
)

func multipleImports() {
	_ = fmt.Sprint(Pi)
	_ = []byte(nil)
}
