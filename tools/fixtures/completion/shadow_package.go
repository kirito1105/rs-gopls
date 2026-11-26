package completion

type PackageAppend struct{ PackageField int }

var append PackageAppend

func packageShadow() {
	_ = append.PackageField
}

func localShadow() {
	append := struct{ LocalField int }{}
	_ = append.LocalField
}
