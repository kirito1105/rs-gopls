package completion

import "example.test/vendordep/visible"

var SiblingPublic = visible.VendorWitness()
var siblingPrivate = 2

func ProductionOnly() int { return SiblingPublic + siblingPrivate }

func init() {}
