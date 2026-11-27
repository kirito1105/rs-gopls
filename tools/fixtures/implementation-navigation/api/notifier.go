package api

// Notifier is implemented by email.Sender and *sms.Sender.
type Notifier interface {
	Notify(message string) string
}
