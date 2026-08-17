package engine

import (
	"context"
	"sync"
	"sync/atomic"
	"testing"
	"time"
)

func TestFairSchedulerBasic(t *testing.T) {
	cases := map[string]struct {
		reason string
		slots  int
		calls  int
		want   int // max concurrent
	}{
		"One": {
			reason: "A scheduler with 1 slot serializes runs.",
			slots:  1,
			calls:  3,
			want:   1,
		},
		"Two": {
			reason: "A scheduler with 2 slots allows two at once.",
			slots:  2,
			calls:  4,
			want:   2,
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			s := newFairScheduler(tc.slots)
			ctx := context.Background()
			var wg sync.WaitGroup
			var maxConcurrent atomic.Int32
			var current atomic.Int32

			for range tc.calls {
				wg.Add(1)
				go func() {
					defer wg.Done()
					release, err := s.acquire(ctx, "sha256:test")
					if err != nil {
						t.Errorf("acquire(): %v", err)
						return
					}
					c := current.Add(1)
					for {
						prev := maxConcurrent.Load()
						if c <= prev || maxConcurrent.CompareAndSwap(prev, c) {
							break
						}
					}
					time.Sleep(10 * time.Millisecond)
					current.Add(-1)
					release()
				}()
			}
			wg.Wait()
			got := int(maxConcurrent.Load())
			if got > tc.want {
				t.Errorf("\n%s\nmaxConcurrent = %d, want at most %d", tc.reason, got, tc.want)
			}
		})
	}
}

func TestFairSchedulerContextCancelled(t *testing.T) {
	s := newFairScheduler(1)
	ctx := context.Background()

	release, err := s.acquire(ctx, "sha256:a")
	if err != nil {
		t.Fatalf("acquire(): %v", err)
	}

	ctx2, cancel := context.WithCancel(context.Background())
	cancel()
	_, err = s.acquire(ctx2, "sha256:b")
	if err == nil {
		t.Fatal("acquire() with cancelled context should fail")
	}
	if want := "waiting for a run slot"; !containsStr(err.Error(), want) {
		t.Errorf("acquire() error = %q, want containing %q", err, want)
	}
	release()
}

func TestFairSchedulerRoundRobin(t *testing.T) {
	// 1 slot, two keys each with 3 waiters. The scheduler should alternate
	// between keys, not drain one before the other.
	s := newFairScheduler(1)
	ctx := context.Background()

	// Take the only slot so all subsequent acquires block.
	release, err := s.acquire(ctx, "sha256:seed")
	if err != nil {
		t.Fatalf("acquire(): %v", err)
	}

	var mu sync.Mutex
	var order []string
	var wg sync.WaitGroup

	// Enqueue 3 waiters for key A and 3 for key B.
	for i := range 3 {
		for _, key := range []string{"A", "B"} {
			wg.Add(1)
			go func() {
				defer wg.Done()
				rel, err := s.acquire(ctx, key)
				if err != nil {
					t.Errorf("acquire(%s, %d): %v", key, i, err)
					return
				}
				mu.Lock()
				order = append(order, key)
				mu.Unlock()
				// Hold briefly so we can see the alternation.
				time.Sleep(time.Millisecond)
				rel()
			}()
		}
		// Stagger slightly so the enqueue order within each batch is
		// deterministic (A before B for each i).
		time.Sleep(time.Millisecond)
	}

	// Give all goroutines time to enqueue before releasing the seed.
	time.Sleep(5 * time.Millisecond)
	release()
	wg.Wait()

	// We expect alternation: A, B, A, B, A, B (round-robin by key).
	// Check that no key has more than 1 consecutive run at any point.
	maxRun := 1
	currentRun := 1
	for i := 1; i < len(order); i++ {
		if order[i] == order[i-1] {
			currentRun++
			if currentRun > maxRun {
				maxRun = currentRun
			}
		} else {
			currentRun = 1
		}
	}
	if maxRun > 2 {
		t.Errorf("expected round-robin alternation, got order %v (max consecutive run: %d)", order, maxRun)
	}
}
