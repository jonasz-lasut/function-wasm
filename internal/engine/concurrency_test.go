package engine

import (
	"context"
	"sync"
	"sync/atomic"
	"testing"
	"time"
)

func TestStepSlots(t *testing.T) {
	cases := map[string]struct {
		reason string
		n      int
		calls  int
		want   int // max concurrent observed
	}{
		"One": {
			reason: "A concurrency of 1 serializes runs.",
			n:      1,
			calls:  3,
			want:   1,
		},
		"Two": {
			reason: "A concurrency of 2 allows two at once.",
			n:      2,
			calls:  4,
			want:   2,
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			s := NewStepSlots()
			ctx := context.Background()
			var wg sync.WaitGroup
			var maxConcurrent atomic.Int32
			var current atomic.Int32

			for range tc.calls {
				wg.Add(1)
				go func() {
					defer wg.Done()
					release, err := s.Acquire(ctx, "sha256:test", tc.n)
					if err != nil {
						t.Errorf("Acquire(): %v", err)
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

func TestStepSlotsContextCancelled(t *testing.T) {
	s := NewStepSlots()
	ctx := context.Background()

	// Fill the only slot.
	release, err := s.Acquire(ctx, "sha256:test", 1)
	if err != nil {
		t.Fatalf("Acquire(): %v", err)
	}

	// A second acquire with a cancelled context fails immediately.
	ctx2, cancel := context.WithCancel(context.Background())
	cancel()
	_, err = s.Acquire(ctx2, "sha256:test", 1)
	if err == nil {
		t.Fatal("Acquire() with cancelled context should fail")
	}
	if want := "waiting for one of this step's 1 run slots (limits.concurrency)"; !containsStr(err.Error(), want) {
		t.Errorf("Acquire() error = %q, want containing %q", err, want)
	}
	release()
}

func TestStepSlotsSweepIdle(t *testing.T) {
	s := NewStepSlots()
	ctx := context.Background()
	release, err := s.Acquire(ctx, "sha256:stale", 1)
	if err != nil {
		t.Fatalf("Acquire(): %v", err)
	}
	release()

	// Backdate the entry.
	s.mu.Lock()
	s.entries["sha256:stale"].lastSeen = time.Now().Add(-stepIdleExpiry - time.Second)
	s.mu.Unlock()

	s.SweepIdle()

	s.mu.Lock()
	_, exists := s.entries["sha256:stale"]
	s.mu.Unlock()
	if exists {
		t.Error("SweepIdle should have removed the stale entry")
	}
}

func containsStr(s, sub string) bool {
	return len(s) >= len(sub) && searchStr(s, sub)
}

func searchStr(s, sub string) bool {
	for i := 0; i <= len(s)-len(sub); i++ {
		if s[i:i+len(sub)] == sub {
			return true
		}
	}
	return false
}
