package importsedit

import append "strings"

func importShadow() {
	_ = append.TrimSpace
}

func localImportShadow() {
	append := struct{ LocalField int }{}
	_ = append.LocalField
}
