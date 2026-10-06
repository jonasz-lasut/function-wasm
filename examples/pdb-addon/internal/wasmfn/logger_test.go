package wasmfn

import (
	"errors"
	"testing"

	"github.com/google/go-cmp/cmp"
)

type sunk struct {
	level   int32
	payload string
}

func TestLogger(t *testing.T) {
	var got []sunk
	logSink = func(level int32, payload []byte) {
		got = append(got, sunk{level: level, payload: string(payload)})
	}
	t.Cleanup(func() { logSink = stderrSink })

	log := NewLogger().WithValues("module", "hello")
	log.Info("Running", "count", 3, "err", errors.New("nope"))
	log.Debug("Details", "ok", true, "raw", map[string]any{"a": []int{1}})
	log.WithValues("more", 1.5).Info("Nested")
	log.Info("Odd", "dangling")

	want := []sunk{
		{level: levelInfo, payload: `{"msg":"Running","kv":["module","hello","count",3,"err","nope"]}`},
		{level: levelDebug, payload: `{"msg":"Details","kv":["module","hello","ok",true,"raw",{"a":[1]}]}`},
		{level: levelInfo, payload: `{"msg":"Nested","kv":["module","hello","more",1.5]}`},
		{level: levelInfo, payload: `{"msg":"Odd","kv":["module","hello","dangling"]}`},
	}
	if diff := cmp.Diff(want, got, cmp.AllowUnexported(sunk{})); diff != "" {
		t.Errorf("logger records: -want, +got:\n%s", diff)
	}
}
