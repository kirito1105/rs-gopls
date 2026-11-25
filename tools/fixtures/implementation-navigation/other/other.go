package other

import navigation "example.com/implementation-navigation"

type OtherSecret struct{}

func (OtherSecret) secretPhaseFour() {}

type EmbeddedSecret struct{ navigation.Secret }
