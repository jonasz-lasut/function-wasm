package wasmfn

import (
	"errors"
	"testing"

	"github.com/google/go-cmp/cmp"
)

type sunk struct {
	level Level
	msg   string
	pairs [][2]string
}

// TestLogger pins what reaches the world's log import: the level, the
// message and the keys and values rendered to string pairs.
func TestLogger(t *testing.T) {
	var got []sunk
	logSink = func(level Level, msg string, pairs [][2]string) {
		got = append(got, sunk{level: level, msg: msg, pairs: pairs})
	}
	t.Cleanup(func() { logSink = hostLogSink })

	log := NewLogger().WithValues("module", "hello")
	log.Info("Running", "count", 3, "err", errors.New("nope"))
	log.Debug("Details", "ok", true, "raw", map[string]any{"a": []int{1}})
	log.WithValues("more", 1.5).Info("Nested")
	log.Info("Odd", "dangling")

	want := []sunk{
		{level: LevelInfo, msg: "Running", pairs: [][2]string{{"module", "hello"}, {"count", "3"}, {"err", "nope"}}},
		{level: LevelDebug, msg: "Details", pairs: [][2]string{{"module", "hello"}, {"ok", "true"}, {"raw", `{"a":[1]}`}}},
		{level: LevelInfo, msg: "Nested", pairs: [][2]string{{"module", "hello"}, {"more", "1.5"}}},
		{level: LevelInfo, msg: "Odd", pairs: [][2]string{{"module", "hello"}, {"dangling", ""}}},
	}
	if diff := cmp.Diff(want, got, cmp.AllowUnexported(sunk{})); diff != "" {
		t.Errorf("logger records: -want, +got:\n%s", diff)
	}
}
