package sms

type Sender struct{}

func (*Sender) Notify(message string) string {
	return "sms: " + message
}
