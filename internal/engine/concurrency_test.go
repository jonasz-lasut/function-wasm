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

func TestStepSlotsCapacityPinnedPerKey(t *testing.T) {
	// Two steps naming the same digest with different limits.concurrency must
	// not add up: the first-seen count governs, so the extra slots the larger
	// n asks for are never admitted while the first-seen slots are held.
	s := NewStepSlots()
	ctx := context.Background()

	const firstSeen = 2
	held := make([]func(), 0, firstSeen)
	for range firstSeen {
		release, err := s.Acquire(ctx, "sha256:test", firstSeen)
		if err != nil {
			t.Fatalf("Acquire(): %v", err)
		}
		held = append(held, release)
	}

	// A larger n on the same key must not open a fresh channel: with every
	// first-seen slot held, this Acquire blocks until its deadline.
	blocked, cancel := context.WithTimeout(context.Background(), 50*time.Millisecond)
	defer cancel()
	if _, err := s.Acquire(blocked, "sha256:test", firstSeen+3); err == nil {
		t.Fatal("Acquire() with a larger n admitted past the first-seen capacity")
	}

	// Releasing a first-seen slot lets exactly one more in, proving the
	// larger n reused the existing channel rather than a new one.
	held[0]()
	admitted, cancelAdmit := context.WithTimeout(context.Background(), time.Second)
	defer cancelAdmit()
	release, err := s.Acquire(admitted, "sha256:test", firstSeen+3)
	if err != nil {
		t.Fatalf("Acquire() after a release should have been admitted: %v", err)
	}
	release()
	for _, r := range held[1:] {
		r()
	}
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

func TestStepSlotsSweepIdleSkipsHeld(t *testing.T) {
	// An entry whose slot is currently held must survive the sweep even with an
	// old lastSeen: removing it would let the next Acquire build a fresh channel
	// beside the one the live holder drains, re-opening the over-admission.
	s := NewStepSlots()
	ctx := context.Background()
	release, err := s.Acquire(ctx, "sha256:held", 1)
	if err != nil {
		t.Fatalf("Acquire(): %v", err)
	}

	// Backdate the held entry so only the len(e.ch) guard keeps it.
	s.mu.Lock()
	s.entries["sha256:held"].lastSeen = time.Now().Add(-stepIdleExpiry - time.Second)
	s.mu.Unlock()

	s.SweepIdle()

	s.mu.Lock()
	_, exists := s.entries["sha256:held"]
	s.mu.Unlock()
	if !exists {
		t.Error("SweepIdle removed an entry that still holds a slot")
	}

	// Once released and still stale, the same entry is idle and swept.
	release()
	s.mu.Lock()
	s.entries["sha256:held"].lastSeen = time.Now().Add(-stepIdleExpiry - time.Second)
	s.mu.Unlock()

	s.SweepIdle()

	s.mu.Lock()
	_, exists = s.entries["sha256:held"]
	s.mu.Unlock()
	if exists {
		t.Error("SweepIdle should have removed the released, stale entry")
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
