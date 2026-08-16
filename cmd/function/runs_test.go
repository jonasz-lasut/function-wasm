package main

import (
	"context"
	"os"
	"path/filepath"
	"strconv"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/testing/protocmp"

	xplogging "github.com/crossplane/crossplane-runtime/v2/pkg/logging"
	fnv1 "github.com/crossplane/function-sdk-go/proto/v1"

	"github.com/jonasz-lasut/function-wasm/internal/engine"
	"github.com/jonasz-lasut/function-wasm/internal/module"
	"github.com/jonasz-lasut/function-wasm/internal/testwasm"
)

// gate is a logger a guest blocks on: a run whose guest logs "hold" reports
// itself on entered and waits for release, so the test knows when a run
// holds its slot and decides when it gives it back. Every other line is
// dropped.
type gate struct {
	entered chan struct{}
	release chan struct{}
}

func (g *gate) Info(msg string, _ ...any) {
	if msg == "hold" {
		g.entered <- struct{}{}
		<-g.release
	}
}
func (g *gate) Debug(string, ...any)               {}
func (g *gate) WithValues(...any) xplogging.Logger { return g }

// TestRunFunctionConcurrentRuns pins --max-concurrent-runs at the function:
// with one slot two requests serialise, and a request whose deadline passes
// while it waits is a fatal result naming the wait.
func TestRunFunctionConcurrentRuns(t *testing.T) {
	// A guest that logs "hold" (blocking on the gate) and then returns its
	// response: the JSON record sits below the response and heap offsets
	// testwasm uses, the response is where testwasm.Fixed puts it.
	raw, err := proto.Marshal(guestResponse())
	if err != nil {
		t.Fatal(err)
	}
	hold := testwasm.Fixed(t, guestResponse(), testwasm.Options{
		Extra: `(import "wasmfn" "log" (func $log (param i32 i32 i32)))
  (data (i32.const 16) "{\"msg\":\"hold\"}")`,
		Body: `(call $log (i32.const 0) (i32.const 16) (i32.const 14))
    (i64.or (i64.shl (i64.const 1024) (i64.const 32)) (i64.const ` + strconv.Itoa(len(raw)) + `))`,
	})
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "hold.wasm"), hold, 0o600); err != nil {
		t.Fatal(err)
	}
	eng, err := engine.New(engine.Config{MaxConcurrentRuns: 1})
	if err != nil {
		t.Fatal(err)
	}
	defer eng.Close()
	resolver, err := module.NewResolver(module.Options{Dir: dir})
	if err != nil {
		t.Fatal(err)
	}
	g := &gate{entered: make(chan struct{}), release: make(chan struct{})}
	f := &Function{log: g, ttl: ttl, engine: eng, modules: engine.NewCache(eng, engine.CacheOptions{}), resolver: resolver}
	req := func() *fnv1.RunFunctionRequest {
		return &fnv1.RunFunctionRequest{Meta: &fnv1.RequestMeta{Tag: "hello"}, Input: input(t, pathModule("hold.wasm"))}
	}
	type result struct {
		rsp *fnv1.RunFunctionResponse
		err error
	}
	start := func(ctx context.Context) <-chan result {
		out := make(chan result, 1)
		go func() {
			rsp, err := f.RunFunction(ctx, req())
			out <- result{rsp: rsp, err: err}
		}()
		return out
	}

	first := start(context.Background())
	<-g.entered // the first request holds the only slot, inside the guest

	// Generous enough for the request path up to the slot wait even on a
	// loaded machine, so the deadline can only pass while waiting there.
	ctx, cancel := context.WithTimeout(context.Background(), 750*time.Millisecond)
	defer cancel()
	rsp, err := f.RunFunction(ctx, req())
	if err != nil {
		t.Fatalf("RunFunction() while the slot is held: %v", err)
	}
	if diff := cmp.Diff(fatal("module module file hold.wasm failed: waiting for a run slot: context deadline exceeded"), rsp, protocmp.Transform()); diff != "" {
		t.Errorf("a request whose deadline passes while waiting for a slot: -want, +got:\n%s", diff)
	}

	second := start(context.Background())
	select {
	case <-g.entered:
		t.Fatal("the second request entered the guest while the first held the only slot")
	case r := <-second:
		t.Fatalf("the second request finished while the first held the only slot: %+v", r)
	case <-time.After(50 * time.Millisecond):
	}

	close(g.release)
	for _, r := range []<-chan result{first, second} {
		if r == second {
			<-g.entered
		}
		got := <-r
		if got.err != nil {
			t.Fatalf("RunFunction(): %v", got.err)
		}
		if diff := cmp.Diff(guestResponse(), got.rsp, protocmp.Transform()); diff != "" {
			t.Errorf("a request that waited for its slot runs like any other: -want, +got:\n%s", diff)
		}
	}
}
