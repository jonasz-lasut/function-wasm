//go:build !wasip1

package wasmfn

import (
	"fmt"
	"os"
	"strings"
)

// hostLogSink prints to stderr outside a wasip1 build so native tests see
// the guest's log lines.
func hostLogSink(level Level, msg string, pairs [][2]string) {
	var b strings.Builder
	for _, kv := range pairs {
		fmt.Fprintf(&b, " %s=%q", kv[0], kv[1])
	}
	fmt.Fprintf(os.Stderr, "wasmfn %s %q%s\n", levelName(level), msg, b.String())
}

func levelName(level Level) string {
	switch level {
	case LevelDebug:
		return "debug"
	case LevelWarn:
		return "warn"
	case LevelError:
		return "error"
	default:
		return "info"
	}
}

// hostHTTPCall has no host to ask outside a wasip1 build; native tests of a
// guest see ErrNoHostHTTP unless they replace httpCall.
func hostHTTPCall(hostRequest) (*hostResponse, error) {
	return nil, ErrNoHostHTTP
}
