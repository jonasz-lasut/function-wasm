package engine

import (
	"context"
	"fmt"
	"sync"
	"time"
)

// StepSlots bounds how many runs of a given key execute at once. The key
// is the caller's: typically the module digest, so one module does not
// take every global run slot from every other. Idle entries are swept by
// SweepIdle.
type StepSlots struct {
	mu      sync.Mutex
	entries map[string]*stepEntry
}

type stepEntry struct {
	ch       chan struct{}
	lastSeen time.Time
}

// stepIdleExpiry is how long an unused entry stays before SweepIdle
// removes it: ten minutes, matching the memory cache's idle TTL.
const stepIdleExpiry = 10 * time.Minute

// NewStepSlots returns a StepSlots.
func NewStepSlots() *StepSlots {
	return &StepSlots{entries: make(map[string]*stepEntry)}
}

// Acquire waits for one of the key's n slots under ctx. The returned
// function releases the slot; call it exactly once. An error means ctx
// ended before a slot was free, and no slot is held.
func (s *StepSlots) Acquire(ctx context.Context, key string, n int) (release func(), err error) {
	s.mu.Lock()
	e, ok := s.entries[key]
	if !ok || cap(e.ch) != n {
		// First use or concurrency changed: allocate a fresh channel.
		e = &stepEntry{ch: make(chan struct{}, n)}
		s.entries[key] = e
	}
	e.lastSeen = time.Now()
	s.mu.Unlock()

	select {
	case e.ch <- struct{}{}:
		return func() { <-e.ch }, nil
	case <-ctx.Done():
		return nil, fmt.Errorf("waiting for one of this step's %d run slots (limits.concurrency): %w", n, ctx.Err())
	}
}

// SweepIdle removes entries not seen for stepIdleExpiry.
func (s *StepSlots) SweepIdle() {
	s.mu.Lock()
	defer s.mu.Unlock()
	cutoff := time.Now().Add(-stepIdleExpiry)
	for k, e := range s.entries {
		if e.lastSeen.Before(cutoff) {
			delete(s.entries, k)
		}
	}
}
