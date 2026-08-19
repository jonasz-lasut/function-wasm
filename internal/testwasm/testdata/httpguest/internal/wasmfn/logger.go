package wasmfn

import (
	"encoding/json"
	"fmt"

	"github.com/crossplane/crossplane-runtime/v2/pkg/logging"
)

// Log levels of the wasmfn.log host import.
const (
	levelInfo  int32 = 0
	levelDebug int32 = 1
)

// logSink delivers one log record to the host. The wasip1 build wires it to
// the wasmfn.log import; other builds print to stderr so native tests can run
// the same code.
var logSink = stderrSink

// record is the JSON payload of one wasmfn.log call.
type record struct {
	Msg string `json:"msg"`
	// KV are alternating keys and values, as logr and function-sdk-go's
	// logging.Logger take them.
	KV []any `json:"kv,omitempty"`
}

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
	l.emit(levelInfo, msg, keysAndValues)
}

func (l *logger) Debug(msg string, keysAndValues ...any) {
	l.emit(levelDebug, msg, keysAndValues)
}

func (l *logger) WithValues(keysAndValues ...any) logging.Logger {
	return &logger{kv: l.merge(keysAndValues)}
}

// merge returns this logger's keys and values followed by the call's. It always
// allocates a fresh slice, never aliasing l.kv, so emit can rewrite its copy in
// place without disturbing the shared parent slice.
func (l *logger) merge(keysAndValues []any) []any {
	kv := make([]any, 0, len(l.kv)+len(keysAndValues))
	kv = append(kv, l.kv...)
	kv = append(kv, keysAndValues...)
	return kv
}

func (l *logger) emit(level int32, msg string, keysAndValues []any) {
	kv := l.merge(keysAndValues)
	for i := range kv {
		kv[i] = jsonable(kv[i])
	}
	payload, err := json.Marshal(record{Msg: msg, KV: kv})
	if err != nil {
		// jsonable makes this unreachable for ordinary values; still never
		// drop the message.
		payload = []byte(fmt.Sprintf(`{"msg":%q,"kv":["wasmfn-log-error",%q]}`, msg, err.Error()))
	}
	logSink(level, payload)
}

// jsonable keeps values the host can decode: JSON-native types pass through,
// errors and everything else are stringified the way structured loggers do.
func jsonable(v any) any {
	switch t := v.(type) {
	case nil, bool, string, float32, float64, int, int8, int16, int32, int64, uint, uint8, uint16, uint32, uint64:
		return t
	case error:
		return t.Error()
	case fmt.Stringer:
		return t.String()
	}
	if _, err := json.Marshal(v); err == nil {
		return v
	}
	return fmt.Sprintf("%v", v)
}
