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

func TestFairSchedulerNoLeakOnAdmitCancelRace(t *testing.T) {
	// Regression: wakeNext admits a waiter (s.active++, signal on its channel)
	// while, in the same instant, that waiter's context fires. If the grant is
	// dropped on the cancel path, s.active climbs until it equals total with no
	// real holder and the scheduler deadlocks for good. Storm the admit/cancel
	// race, then prove the scheduler reconciled every grant and still works.
	s := newFairScheduler(1)

	// Run the storm in a goroutine so an unfixed scheduler that wedges fails
	// the test by timeout instead of hanging the whole suite.
	done := make(chan struct{})
	go func() {
		defer close(done)

		// A driver churns the single slot: each release admits a victim
		// waiter through wakeNext, which is where a simultaneously-cancelling
		// waiter used to leak a slot.
		driverCtx, stopDriver := context.WithCancel(context.Background())
		var driverWG sync.WaitGroup
		driverWG.Add(1)
		go func() {
			defer driverWG.Done()
			for driverCtx.Err() == nil {
				rel, err := s.acquire(driverCtx, "driver")
				if err != nil {
					return // driver context ended while waiting
				}
				rel()
			}
		}()

		// Victims on a different key with very short, index-jittered deadlines,
		// so a good number expire at almost the instant wakeNext admits them.
		// A bounded worker pool keeps the queue small, so an admitted victim is
		// always a freshly-enqueued one whose deadline is near its admission.
		const iterations = 10000
		inFlight := make(chan struct{}, 16)
		var victimWG sync.WaitGroup
		for i := range iterations {
			inFlight <- struct{}{}
			victimWG.Add(1)
			go func() {
				defer victimWG.Done()
				defer func() { <-inFlight }()
				d := time.Duration(30+(i%50)) * time.Microsecond
				ctx, cancel := context.WithTimeout(context.Background(), d)
				defer cancel()
				if rel, err := s.acquire(ctx, "victim"); err == nil {
					rel()
				}
			}()
		}
		victimWG.Wait()

		stopDriver()
		driverWG.Wait()
	}()

	select {
	case <-done:
	case <-time.After(30 * time.Second):
		t.Fatal("scheduler storm did not finish in time: a leaked slot deadlocked acquire")
	}

	// Every admit/cancel race must have reconciled: no slot is held now.
	s.mu.Lock()
	active := s.active
	s.mu.Unlock()
	if active != 0 {
		t.Fatalf("active = %d after quiesce, want 0 (a run slot leaked)", active)
	}

	// And a fresh acquire is granted promptly rather than blocking forever.
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	rel, err := s.acquire(ctx, "after")
	if err != nil {
		t.Fatalf("acquire after quiesce: %v (the scheduler is wedged)", err)
	}
	if rel == nil {
		t.Fatal("acquire after quiesce returned a nil release func")
	}
	rel()
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
