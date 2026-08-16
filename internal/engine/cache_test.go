package engine

import (
	"errors"
	"sync"
	"sync/atomic"
	"testing"

	"github.com/google/go-cmp/cmp"

	"github.com/jonasz-lasut/function-wasm/internal/metrics"
)

func TestCache(t *testing.T) {
	type step struct {
		digest string
		err    error
	}
	type want struct {
		loads map[string]int
		len   int
	}
	cases := map[string]struct {
		reason string
		size   int
		steps  []step
		want   want
	}{
		"HitAfterMiss": {
			reason: "The second Get for a digest does not load again.",
			size:   2,
			steps:  []step{{digest: "a"}, {digest: "a"}},
			want:   want{loads: map[string]int{"a": 1}, len: 1},
		},
		"EvictsLeastRecentlyUsed": {
			reason: "Touching a keeps it hot; b is evicted when c arrives, so b loads twice.",
			size:   2,
			steps:  []step{{digest: "a"}, {digest: "b"}, {digest: "a"}, {digest: "c"}, {digest: "b"}},
			want:   want{loads: map[string]int{"a": 1, "b": 2, "c": 1}, len: 2},
		},
		"FailedLoadNotCached": {
			reason: "A load error is returned but the digest is retried next time.",
			size:   2,
			steps:  []step{{digest: "a", err: errors.New("boom")}, {digest: "a"}},
			want:   want{loads: map[string]int{"a": 2}, len: 1},
		},
		"MinimumSizeOne": {
			reason: "A size below one still caches one module.",
			size:   0,
			steps:  []step{{digest: "a"}, {digest: "a"}},
			want:   want{loads: map[string]int{"a": 1}, len: 1},
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			c := NewCache(tc.size)
			loads := map[string]int{}
			for _, s := range tc.steps {
				_, err := c.Get(s.digest, func() (*Module, error) {
					loads[s.digest]++
					if s.err != nil {
						return nil, s.err
					}
					return &Module{}, nil
				})
				if !errors.Is(err, s.err) {
					t.Fatalf("\n%s\nGet(%q): want error %v, got %v", tc.reason, s.digest, s.err, err)
				}
			}
			if diff := cmp.Diff(tc.want.loads, loads); diff != "" {
				t.Errorf("\n%s\nloads: -want, +got:\n%s", tc.reason, diff)
			}
			if diff := cmp.Diff(tc.want.len, c.Len()); diff != "" {
				t.Errorf("\n%s\nLen(): -want, +got:\n%s", tc.reason, diff)
			}
		})
	}
}

func TestCacheMetrics(t *testing.T) {
	hits, _ := metrics.Sample("function_wasm_module_cache_events_total", map[string]string{"cache": metrics.CacheCompiled, "event": metrics.EventHit})
	misses, _ := metrics.Sample("function_wasm_module_cache_events_total", map[string]string{"cache": metrics.CacheCompiled, "event": metrics.EventMiss})
	c := NewCache(2)
	for _, d := range []string{"a", "a", "b"} {
		_, _ = c.Get(d, func() (*Module, error) { return &Module{}, nil })
	}
	if got, _ := metrics.Sample("function_wasm_module_cache_events_total", map[string]string{"cache": metrics.CacheCompiled, "event": metrics.EventHit}); got != hits+1 {
		t.Errorf("compiled cache hits: want %v, got %v", hits+1, got)
	}
	if got, _ := metrics.Sample("function_wasm_module_cache_events_total", map[string]string{"cache": metrics.CacheCompiled, "event": metrics.EventMiss}); got != misses+2 {
		t.Errorf("compiled cache misses: want %v, got %v", misses+2, got)
	}
}

func TestCacheSingleFlight(t *testing.T) {
	c := NewCache(4)
	var loads atomic.Int32
	release := make(chan struct{})
	var wg sync.WaitGroup
	for range 16 {
		wg.Go(func() {
			_, _ = c.Get("a", func() (*Module, error) {
				loads.Add(1)
				<-release
				return &Module{}, nil
			})
		})
	}
	// Give every goroutine the chance to arrive before the load completes.
	for c.Len() == 0 && loads.Load() == 0 {
	}
	close(release)
	wg.Wait()
	if got := loads.Load(); got != 1 {
		t.Errorf("concurrent Get() for one digest loaded %d times, want 1", got)
	}
}
