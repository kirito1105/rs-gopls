package email

type Sender struct{}

func (Sender) Notify(message string) string {
	return "email: " + message
}
