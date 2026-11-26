package actions

import (
	"strings"
	"example.test/actions/helper" // Matches the labels used by built-in commands.
	"os"
	// The shared prefix keeps labels consistent across commands.
	dependency "example.test/dependency"
	"fmt"
	_ "time/tzdata"
	"strings"
)

func ImportedLabel(input string) string {
	return fmt.Sprint(dependency.Prefix, helper.Label(), strings.TrimSpace(input))
}
