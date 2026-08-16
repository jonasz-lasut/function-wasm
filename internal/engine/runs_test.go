package engine

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"google.golang.org/protobuf/testing/protocmp"

	xplogging "github.com/crossplane/crossplane-runtime/v2/pkg/logging"
	fnv1 "github.com/crossplane/function-sdk-go/proto/v1"

	"github.com/jonasz-lasut/function-wasm/internal/metrics"
	"github.com/jonasz-lasut/function-wasm/internal/testwasm"
)

// gate is a logger a guest blocks on: a run whose guest logs "hold" reports
// itself on entered and waits for release, so a test knows exactly when a
// run holds its slot and decides when it gives it back.
type gate struct {
	entered chan struct{}
	release chan struct{}
}

func newGate() *gate {
	return &gate{entered: make(chan struct{}), release: make(chan struct{})}
}

func (g *gate) Info(msg string, _ ...any) {
	if msg == "hold" {
		g.entered <- struct{}{}
		<-g.release
	}
}
func (g *gate) Debug(string, ...any)               {}
func (g *gate) WithValues(...any) xplogging.Logger { return g }

type runResult struct {
	rsp *fnv1.RunFunctionResponse
	err error
}

// holdModule is a guest that logs "hold" (blocking on a gate logger) and
// then returns an empty response.
func holdModule(t *testing.T, e *Engine) *Module {
	t.Helper()
	extra, body := logImport(0, `{\"msg\":\"hold\"}`)
	m, err := e.Compile(testwasm.Fixed(t, cannedResponse(), testwasm.Options{Extra: extra, Body: body + "(i64.const 0)"}))
	if err != nil {
		t.Fatalf("Compile(): %v", err)
	}
	return m
}

func inFlight(t *testing.T) float64 {
	t.Helper()
	v, ok := metrics.Sample("function_wasm_module_runs_in_flight", nil)
	if !ok {
		t.Fatal("runs_in_flight gauge is not registered")
	}
	return v
}

// TestRunSlots pins the concurrent-runs bound: with one slot a second run
// waits for the first, a waiter whose context ends first fails without
// running, and without a bound runs overlap.
func TestRunSlots(t *testing.T) {
	t.Run("Bounded", func(t *testing.T) {
		e, err := New(Config{MaxConcurrentRuns: 1})
		if err != nil {
			t.Fatal(err)
		}
		defer e.Close()
		m := holdModule(t, e)
		g := newGate()
		start := func(ctx context.Context) <-chan runResult {
			out := make(chan runResult, 1)
			go func() {
				rsp, err := e.Run(ctx, m, request(), g, RunOptions{})
				out <- runResult{rsp: rsp, err: err}
			}()
			return out
		}

		first := start(context.Background())
		<-g.entered
		if got := inFlight(t); got != 1 {
			t.Fatalf("runs_in_flight while one run holds the slot: want 1, got %v", got)
		}

		// A waiter whose deadline passes while the slot is held fails
		// naming the wait, not the module.
		ctx, cancel := context.WithTimeout(context.Background(), 50*time.Millisecond)
		defer cancel()
		_, err = e.Run(ctx, m, request(), g, RunOptions{})
		if !errors.Is(err, context.DeadlineExceeded) || err.Error() != "waiting for a run slot: context deadline exceeded" {
			t.Fatalf("waiter past its deadline: want 'waiting for a run slot: context deadline exceeded', got %v", err)
		}

		// A patient second run waits: it does not enter the guest while the
		// first holds the slot.
		second := start(context.Background())
		select {
		case <-g.entered:
			t.Fatal("second run entered the guest while the first held the only slot")
		case r := <-second:
			t.Fatalf("second run finished while the first held the only slot: %+v", r)
		case <-time.After(50 * time.Millisecond):
		}
		if got := inFlight(t); got != 1 {
			t.Fatalf("runs_in_flight with one run holding the slot and one waiting: want 1, got %v", got)
		}

		close(g.release)
		if r := <-first; r.err != nil {
			t.Fatalf("first run: %v", r.err)
		}
		<-g.entered
		r := <-second
		if r.err != nil {
			t.Fatalf("second run: %v", r.err)
		}
		if diff := cmp.Diff(&fnv1.RunFunctionResponse{}, r.rsp, protocmp.Transform()); diff != "" {
			t.Errorf("second run response: -want, +got:\n%s", diff)
		}
		if got := inFlight(t); got != 0 {
			t.Errorf("runs_in_flight after both runs: want 0, got %v", got)
		}
	})

	t.Run("Unbounded", func(t *testing.T) {
		e, err := New(Config{})
		if err != nil {
			t.Fatal(err)
		}
		defer e.Close()
		m := holdModule(t, e)
		g := newGate()
		results := make(chan runResult, 2)
		for range 2 {
			go func() {
				rsp, err := e.Run(context.Background(), m, request(), g, RunOptions{})
				results <- runResult{rsp: rsp, err: err}
			}()
		}
		// Both enter the guest before either finishes: nothing serialises
		// them.
		<-g.entered
		<-g.entered
		if got := inFlight(t); got != 2 {
			t.Errorf("runs_in_flight with two runs and no bound: want 2, got %v", got)
		}
		close(g.release)
		for range 2 {
			if r := <-results; r.err != nil {
				t.Errorf("run: %v", r.err)
			}
		}
	})
}
