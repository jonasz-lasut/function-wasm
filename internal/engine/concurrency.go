package engine

import (
	"context"
	"fmt"
	"sync"
	"time"
)

// StepSlots bounds how many runs of a given key execute at once. The key
// is the caller's: typically the module digest, so one module does not
// take every global run slot from every other.
//
// The slot count is fixed at a key's first Acquire and governs that key
// until its entry goes idle and SweepIdle removes it. Two Composition steps
// that name the same module digest with different limits.concurrency
// therefore share the first-seen bound: the channel is never replaced while
// runs may hold it, so an in-flight holder is never orphaned onto an old
// channel and the effective concurrency can never exceed the first value.
// This keeps the digest-keyed semantics the request path documents; a
// changed limit takes effect only after the entry is swept. Idle entries
// are swept by SweepIdle.
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
//
// n sizes the channel only on a key's first Acquire. A later Acquire with a
// different n reuses the existing channel rather than replacing it: swapping
// it would leave in-flight holders draining the old channel while new callers
// fill a fresh one, so both bounds would run at once and each would be
// exceeded. First-seen-wins avoids that; the bound changes only once the
// entry is swept.
func (s *StepSlots) Acquire(ctx context.Context, key string, n int) (release func(), err error) {
	s.mu.Lock()
	e, ok := s.entries[key]
	if !ok {
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

// SweepIdle removes entries not seen for stepIdleExpiry. An entry that still
// holds a slot (len(e.ch) > 0) is kept even when its lastSeen is old:
// dropping it would let the next Acquire build a fresh channel while the
// current holder still drains the old one, re-introducing the over-admission
// that pinning the capacity per key prevents. A held entry is by definition
// in use, so it is never truly idle.
func (s *StepSlots) SweepIdle() {
	s.mu.Lock()
	defer s.mu.Unlock()
	cutoff := time.Now().Add(-stepIdleExpiry)
	for k, e := range s.entries {
		if e.lastSeen.Before(cutoff) && len(e.ch) == 0 {
			delete(s.entries, k)
		}
	}
}
