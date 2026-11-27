package caller

import (
	"fmt"

	"example.test/completion/pkg/internal/secret"
)

func Call() string {
	_ = secret.InternalWitness()
	return fmt.Sprint(0)
}
