package engine

import (
	"context"
	"errors"
	"runtime"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/spf13/afero"
	"google.golang.org/protobuf/testing/protocmp"

	"github.com/jonasz-lasut/function-wasm/internal/cache"
	"github.com/jonasz-lasut/function-wasm/internal/metrics"
	"github.com/jonasz-lasut/function-wasm/internal/testwasm"
)

func TestCache(t *testing.T) {
	e, err := New(Config{})
	if err != nil {
		t.Fatal(err)
	}
	defer e.Close()
	wasm := testwasm.Fixed(t, cannedResponse(), testwasm.Options{})

	type step struct {
		digest  string
		advance time.Duration
		err     error
	}
	type want struct {
		fetches map[string]int
		len     int
		onDisk  int
	}
	cases := map[string]struct {
		reason     string
		disk       bool
		noMemory   bool
		maxEntries int
		steps      []step
		want       want
	}{
		"NoMemoryTier": {
			reason:   "With the memory tier off nothing is retained; every request loads the artifact from disk and the module is fetched once.",
			disk:     true,
			noMemory: true,
			steps:    []step{{digest: "a"}, {digest: "a"}, {digest: "a"}},
			want:     want{fetches: map[string]int{"a": 1}, len: 0, onDisk: 1},
		},
		"MemoryHit": {
			reason: "The second Get for a digest within the TTL neither fetches nor touches disk.",
			steps:  []step{{digest: "a"}, {digest: "a"}},
			want:   want{fetches: map[string]int{"a": 1}, len: 1},
		},
		"IdleExpiry": {
			reason: "A module idle for longer than the TTL leaves memory; without a disk store it is fetched again.",
			steps:  []step{{digest: "a"}, {digest: "a", advance: 2 * time.Minute}},
			want:   want{fetches: map[string]int{"a": 2}, len: 1},
		},
		"IdleExpiryServedFromDisk": {
			reason: "With the disk store, an expired module is deserialized, not fetched.",
			disk:   true,
			steps:  []step{{digest: "a"}, {digest: "a", advance: 2 * time.Minute}},
			want:   want{fetches: map[string]int{"a": 1}, len: 1, onDisk: 1},
		},
		"TouchExtendsLife": {
			reason: "Each use restarts the idle clock.",
			steps:  []step{{digest: "a"}, {digest: "a", advance: 40 * time.Second}, {digest: "a", advance: 40 * time.Second}},
			want:   want{fetches: map[string]int{"a": 1}, len: 1},
		},
		"FailedLoadNotCached": {
			reason: "A fetch error is returned and retried next time.",
			disk:   true,
			steps:  []step{{digest: "a", err: errors.New("boom")}, {digest: "a"}},
			want:   want{fetches: map[string]int{"a": 2}, len: 1, onDisk: 1},
		},
		"TwoModules": {
			reason: "Digests are independent entries.",
			disk:   true,
			steps:  []step{{digest: "a"}, {digest: "b"}, {digest: "a"}},
			want:   want{fetches: map[string]int{"a": 1, "b": 1}, len: 2, onDisk: 2},
		},
		"BoundedTier": {
			reason:     "Past MaxEntries the least recently used module leaves memory and comes back from disk when asked for again.",
			disk:       true,
			maxEntries: 2,
			steps:      []step{{digest: "a"}, {digest: "b", advance: time.Second}, {digest: "a", advance: time.Second}, {digest: "c", advance: time.Second}, {digest: "b", advance: time.Second}},
			want:       want{fetches: map[string]int{"a": 1, "b": 1, "c": 1}, len: 2, onDisk: 3},
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			var disk *cache.Store
			if tc.disk {
				disk = cache.New(afero.NewMemMapFs(), false)
			}
			c := NewCache(e, CacheOptions{Disk: disk, IdleTTL: time.Minute, NoMemory: tc.noMemory, MaxEntries: tc.maxEntries})
			now := time.Unix(1_700_000_000, 0)
			c.now = func() time.Time { return now }
			fetches := map[string]int{}
			for _, s := range tc.steps {
				now = now.Add(s.advance)
				m, err := c.Get(context.Background(), s.digest, func(context.Context) ([]byte, error) {
					fetches[s.digest]++
					if s.err != nil {
						return nil, s.err
					}
					return wasm, nil
				})
				if !errors.Is(err, s.err) {
					t.Fatalf("\n%s\nGet(%q): want error %v, got %v", tc.reason, s.digest, s.err, err)
				}
				if err == nil {
					// Whatever tier served it, it runs.
					got, err := e.Run(context.Background(), m, request(), &recorder{})
					m.Release()
					if err != nil {
						t.Fatalf("\n%s\nRun(): %v", tc.reason, err)
					}
					if diff := cmp.Diff(cannedResponse(), got, protocmp.Transform()); diff != "" {
						t.Errorf("\n%s\nRun(): -want, +got:\n%s", tc.reason, diff)
					}
				}
			}
			if diff := cmp.Diff(tc.want.fetches, fetches); diff != "" {
				t.Errorf("\n%s\nfetches: -want, +got:\n%s", tc.reason, diff)
			}
			if diff := cmp.Diff(tc.want.len, c.Len()); diff != "" {
				t.Errorf("\n%s\nLen(): -want, +got:\n%s", tc.reason, diff)
			}
			if disk != nil {
				if diff := cmp.Diff(tc.want.onDisk, disk.Len()); diff != "" {
					t.Errorf("\n%s\nartifacts on disk: -want, +got:\n%s", tc.reason, diff)
				}
			}
		})
	}
}

func TestCacheStaleArtifactIsRecompiled(t *testing.T) {
	e, err := New(Config{})
	if err != nil {
		t.Fatal(err)
	}
	defer e.Close()
	disk := cache.New(afero.NewMemMapFs(), false)
	if err := disk.Put("a", []byte("not a wasmtime artifact")); err != nil {
		t.Fatal(err)
	}
	c := NewCache(e, CacheOptions{Disk: disk, IdleTTL: time.Minute})
	fetched := 0
	if _, err := c.Get(context.Background(), "a", func(context.Context) ([]byte, error) {
		fetched++
		return testwasm.Fixed(t, cannedResponse(), testwasm.Options{}), nil
	}); err != nil {
		t.Fatalf("Get(): %v", err)
	}
	if fetched != 1 {
		t.Errorf("a stale artifact should have caused a fetch, got %d fetches", fetched)
	}
	if b, ok := disk.Get("a"); !ok || string(b) == "not a wasmtime artifact" {
		t.Error("the stale artifact was not replaced by a fresh one")
	}
}

func TestCacheMetrics(t *testing.T) {
	e, err := New(Config{})
	if err != nil {
		t.Fatal(err)
	}
	defer e.Close()
	sample := func(cacheName, event string) float64 {
		v, _ := metrics.Sample("function_wasm_module_cache_events_total", map[string]string{"cache": cacheName, "event": event})
		return v
	}
	memHits, memMisses := sample(metrics.CacheCompiled, metrics.EventHit), sample(metrics.CacheCompiled, metrics.EventMiss)
	diskHits, diskMisses := sample(metrics.CacheCompiledDisk, metrics.EventHit), sample(metrics.CacheCompiledDisk, metrics.EventMiss)

	c := NewCache(e, CacheOptions{Disk: cache.New(afero.NewMemMapFs(), false), IdleTTL: time.Minute})
	now := time.Unix(1_700_000_000, 0)
	c.now = func() time.Time { return now }
	wasm := testwasm.Fixed(t, cannedResponse(), testwasm.Options{})
	fetch := func(context.Context) ([]byte, error) { return wasm, nil }
	_, _ = c.Get(context.Background(), "a", fetch) // memory miss, disk miss, compile
	_, _ = c.Get(context.Background(), "a", fetch) // memory hit
	now = now.Add(2 * time.Minute)
	_, _ = c.Get(context.Background(), "a", fetch) // memory miss, disk hit

	if got := sample(metrics.CacheCompiled, metrics.EventHit); got != memHits+1 {
		t.Errorf("compiled hits: want %v, got %v", memHits+1, got)
	}
	if got := sample(metrics.CacheCompiled, metrics.EventMiss); got != memMisses+2 {
		t.Errorf("compiled misses: want %v, got %v", memMisses+2, got)
	}
	if got := sample(metrics.CacheCompiledDisk, metrics.EventHit); got != diskHits+1 {
		t.Errorf("compiled-disk hits: want %v, got %v", diskHits+1, got)
	}
	if got := sample(metrics.CacheCompiledDisk, metrics.EventMiss); got != diskMisses+1 {
		t.Errorf("compiled-disk misses: want %v, got %v", diskMisses+1, got)
	}
}

func TestCacheSingleFlight(t *testing.T) {
	e, err := New(Config{})
	if err != nil {
		t.Fatal(err)
	}
	defer e.Close()
	c := NewCache(e, CacheOptions{IdleTTL: time.Minute})
	wasm := testwasm.Fixed(t, cannedResponse(), testwasm.Options{})
	var fetches atomic.Int32
	release := make(chan struct{})
	var wg sync.WaitGroup
	for range 16 {
		wg.Go(func() {
			m, err := c.Get(context.Background(), "a", func(context.Context) ([]byte, error) {
				fetches.Add(1)
				<-release
				return wasm, nil
			})
			if err == nil {
				m.Release()
			}
		})
	}
	// Give every goroutine the chance to arrive before the load completes.
	for c.Len() == 0 && fetches.Load() == 0 {
		runtime.Gosched()
	}
	close(release)
	wg.Wait()
	if got := fetches.Load(); got != 1 {
		t.Errorf("concurrent Get() for one digest fetched %d times, want 1", got)
	}
}

func TestCacheLoadPanicDoesNotWedge(t *testing.T) {
	e, err := New(Config{})
	if err != nil {
		t.Fatal(err)
	}
	defer e.Close()
	c := NewCache(e, CacheOptions{IdleTTL: time.Minute})
	func() {
		defer func() { _ = recover() }()
		_, _ = c.Get(context.Background(), "a", func(context.Context) ([]byte, error) { panic("boom") })
	}()
	done := make(chan struct{})
	go func() {
		defer close(done)
		m, err := c.Get(context.Background(), "a", func(context.Context) ([]byte, error) {
			return testwasm.Fixed(t, cannedResponse(), testwasm.Options{}), nil
		})
		if err != nil {
			t.Errorf("Get() after a panicking load: %v", err)
			return
		}
		m.Release()
	}()
	select {
	case <-done:
	case <-time.After(5 * time.Second):
		t.Fatal("a load that panicked left its digest loading forever")
	}
}

func TestCacheWaiterCancel(t *testing.T) {
	e, err := New(Config{})
	if err != nil {
		t.Fatal(err)
	}
	defer e.Close()
	c := NewCache(e, CacheOptions{IdleTTL: time.Minute})
	wasm := testwasm.Fixed(t, cannedResponse(), testwasm.Options{})
	release := make(chan struct{})
	started := make(chan struct{})
	leaderDone := make(chan error, 1)
	go func() {
		m, err := c.Get(context.Background(), "a", func(context.Context) ([]byte, error) {
			close(started)
			<-release
			return wasm, nil
		})
		if err == nil {
			m.Release()
		}
		leaderDone <- err
	}()
	<-started
	// A waiter whose own deadline passes gives up without waiting for the
	// leader's fetch, and without disturbing it.
	ctx, cancel := context.WithTimeout(context.Background(), 50*time.Millisecond)
	defer cancel()
	if _, err := c.Get(ctx, "a", func(context.Context) ([]byte, error) { return wasm, nil }); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("waiter: want %v, got %v", context.DeadlineExceeded, err)
	}
	close(release)
	if err := <-leaderDone; err != nil {
		t.Fatalf("leader: %v", err)
	}
	if c.Len() != 1 {
		t.Errorf("the load should have completed and been kept, Len() = %d", c.Len())
	}
}

func TestCacheLoadOutlivesRequester(t *testing.T) {
	e, err := New(Config{})
	if err != nil {
		t.Fatal(err)
	}
	defer e.Close()
	c := NewCache(e, CacheOptions{IdleTTL: time.Minute})
	wasm := testwasm.Fixed(t, cannedResponse(), testwasm.Options{})
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	// The requester's context is already gone; the load runs under the
	// cache's own and succeeds for whoever asks next.
	m, err := c.Get(ctx, "a", func(ctx context.Context) ([]byte, error) {
		if err := ctx.Err(); err != nil {
			return nil, err
		}
		return wasm, nil
	})
	if err != nil {
		t.Fatalf("Get() with a cancelled requester: %v", err)
	}
	m.Release()
}

func TestCacheCompileSlots(t *testing.T) {
	e, err := New(Config{})
	if err != nil {
		t.Fatal(err)
	}
	defer e.Close()
	c := NewCache(e, CacheOptions{IdleTTL: time.Minute, MaxConcurrentCompiles: 1})
	wasm := testwasm.Fixed(t, cannedResponse(), testwasm.Options{})
	// Hold the only slot from outside, then ask for a module: it must wait
	// for the slot, not compile alongside.
	c.compiles <- struct{}{}
	got := make(chan error, 1)
	go func() {
		m, err := c.Get(context.Background(), "a", func(context.Context) ([]byte, error) { return wasm, nil })
		if err == nil {
			m.Release()
		}
		got <- err
	}()
	select {
	case err := <-got:
		t.Fatalf("Get() finished while the compile slot was taken (err %v)", err)
	case <-time.After(100 * time.Millisecond):
	}
	<-c.compiles
	select {
	case err := <-got:
		if err != nil {
			t.Fatalf("Get(): %v", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("Get() did not proceed once the compile slot was free")
	}
}

func TestCacheMapsArtifactsFromDisk(t *testing.T) {
	e, err := New(Config{})
	if err != nil {
		t.Fatal(err)
	}
	defer e.Close()
	disk, err := cache.OpenDir(t.TempDir(), false)
	if err != nil {
		t.Fatal(err)
	}
	c := NewCache(e, CacheOptions{Disk: disk, IdleTTL: time.Minute, NoMemory: true})
	wasm := testwasm.Fixed(t, cannedResponse(), testwasm.Options{})
	fetches := 0
	for range 3 {
		m, err := c.Get(context.Background(), "a", func(context.Context) ([]byte, error) {
			fetches++
			return wasm, nil
		})
		if err != nil {
			t.Fatal(err)
		}
		got, err := e.Run(context.Background(), m, request(), &recorder{})
		m.Release()
		if err != nil {
			t.Fatal(err)
		}
		if diff := cmp.Diff(cannedResponse(), got, protocmp.Transform()); diff != "" {
			t.Errorf("Run(): -want, +got:\n%s", diff)
		}
	}
	if fetches != 1 {
		t.Errorf("want one fetch, the artifact on disk serving the rest, got %d", fetches)
	}
	if _, ok := disk.Path("a"); !ok {
		t.Error("the artifact should be on disk with a path to map")
	}
}

func TestVersion(t *testing.T) {
	v := Version()
	if !strings.HasPrefix(v, "v") || !strings.HasSuffix(v, "-"+runtime.GOOS+"-"+runtime.GOARCH) {
		t.Errorf("Version() = %q, want <wasmtime-go major from the import path>-<GOOS>-<GOARCH>", v)
	}
}
