// File: ballista/sdks/go/errors.go
package ballista

import (
	"errors"
	"fmt"
	"net/http"
	"time"
)

// ConfigurationError reports invalid client configuration or input rejected before sending.
type ConfigurationError struct {
	Msg string
}

func (e *ConfigurationError) Error() string { return e.Msg }

func configErr(format string, args ...any) error {
	return &ConfigurationError{Msg: fmt.Sprintf(format, args...)}
}

// TransportError reports that the server could not be reached or the connection failed.
// Messages never include the API token.
type TransportError struct {
	Op  string
	Err error
}

func (e *TransportError) Error() string {
	if e.Err == nil {
		return e.Op
	}
	return e.Op + ": " + e.Err.Error()
}

func (e *TransportError) Unwrap() error { return e.Err }

// RunTimeoutError reports that a wait for run completion exceeded its deadline.
type RunTimeoutError struct {
	RunID   string
	Timeout time.Duration
}

func (e *RunTimeoutError) Error() string {
	return fmt.Sprintf("run %s did not finish within %s", e.RunID, e.Timeout)
}

// ResponseError reports a response body that does not match the API contract.
type ResponseError struct {
	What string
}

func (e *ResponseError) Error() string { return "unexpected response: " + e.What }

// APIError is returned when the server answers with an error status.
type APIError struct {
	Status     int
	Code       string
	Detail     string
	RetryAfter time.Duration
}

func (e *APIError) Error() string {
	return fmt.Sprintf("%d %s: %s", e.Status, e.Code, e.Detail)
}

func statusIs(err error, status int) bool {
	var e *APIError
	return errors.As(err, &e) && e.Status == status
}

// IsUnauthorized reports a missing or invalid token.
func IsUnauthorized(err error) bool { return statusIs(err, http.StatusUnauthorized) }

// IsForbidden reports a token whose scope does not allow the operation.
func IsForbidden(err error) bool { return statusIs(err, http.StatusForbidden) }

// IsNotFound reports an unknown bundle, run or artifact.
func IsNotFound(err error) bool { return statusIs(err, http.StatusNotFound) }

// IsConflict reports an operation that conflicts with the current state.
func IsConflict(err error) bool { return statusIs(err, http.StatusConflict) }

// IsValidation reports a plan or request rejected by server-side validation or policy.
func IsValidation(err error) bool { return statusIs(err, http.StatusUnprocessableEntity) }

// IsRateLimited reports a request rejected by the server's rate limit.
func IsRateLimited(err error) bool { return statusIs(err, http.StatusTooManyRequests) }
