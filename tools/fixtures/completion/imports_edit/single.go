// Package importsedit contains import editing samples.
package importsedit

// Formatting support belongs to this declaration.
import formatting `fmt` // Keep this inline comment with fmt.

func singleImport() {
	_ = formatting.Sprint("json")
	_ = []byte(nil)
}
