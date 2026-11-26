package actions


type UnformattedEntry struct {
Name string
        ID int
}


func NewUnformattedEntry() UnformattedEntry {
    return UnformattedEntry{
  Name:"sample",
     ID: 1,
}
}
