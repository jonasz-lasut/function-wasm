package wasmfn

import (
	"encoding/json"
	"fmt"

	"github.com/crossplane/crossplane-runtime/v2/pkg/logging"
)

// Level is a level of the world's typed log import (log-level in
// wasmfn:function): the host renders a line at its own level of the same
// name, and shows debug lines only under --debug.
type Level uint8

// The world's levels, in its declaration order.
const (
	LevelDebug Level = iota
	LevelInfo
	LevelWarn
	LevelError
)

// logSink delivers one log line to the host. The wasip1 build wires it to
// the world's log import; other builds print to stderr so native tests can
// run the same code.
var logSink = hostLogSink

// NewLogger returns a logger that logs through the host. Use it where
// function-template-go's main.go would use function.NewLogger; the value
// satisfies function-sdk-go's logging.Logger, which is defined over this
// interface. crossplane-runtime's package is used directly because
// function-sdk-go's logging package would link zap into every guest.
func NewLogger() logging.Logger {
	return &logger{}
}

type logger struct {
	kv []any
}

func (l *logger) Info(msg string, keysAndValues ...any) {
	l.emit(LevelInfo, msg, keysAndValues)
}

func (l *logger) Debug(msg string, keysAndValues ...any) {
	l.emit(LevelDebug, msg, keysAndValues)
}

func (l *logger) WithValues(keysAndValues ...any) logging.Logger {
	return &logger{kv: l.merge(keysAndValues)}
}

// merge returns this logger's keys and values followed by the call's, in a
// fresh slice that never aliases l.kv.
func (l *logger) merge(keysAndValues []any) []any {
	kv := make([]any, 0, len(l.kv)+len(keysAndValues))
	kv = append(kv, l.kv...)
	kv = append(kv, keysAndValues...)
	return kv
}

// emit renders the alternating keys and values logr-style loggers take into
// the world's list of string pairs; a trailing key without a value gets an
// empty one.
func (l *logger) emit(level Level, msg string, keysAndValues []any) {
	kv := l.merge(keysAndValues)
	pairs := make([][2]string, 0, (len(kv)+1)/2)
	for i := 0; i < len(kv); i += 2 {
		pair := [2]string{stringify(kv[i])}
		if i+1 < len(kv) {
			pair[1] = stringify(kv[i+1])
		}
		pairs = append(pairs, pair)
	}
	logSink(level, msg, pairs)
}

// stringify renders a value the way structured loggers do: strings as they
// are, errors and Stringers by their text, everything else as JSON.
func stringify(v any) string {
	switch t := v.(type) {
	case string:
		return t
	case error:
		return t.Error()
	case fmt.Stringer:
		return t.String()
	}
	if b, err := json.Marshal(v); err == nil {
		return string(b)
	}
	return fmt.Sprintf("%v", v)
}
