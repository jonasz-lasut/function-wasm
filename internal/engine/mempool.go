package engine

import (
	"context"
	"fmt"
	"sync"
)

// memPool is a counting semaphore over bytes: a Run reserves its effective
// memory limit from the pool before it starts and releases it after; a Run
// that cannot fit waits under its context.
type memPool struct {
	total int64

	mu   sync.Mutex
	used int64
	// ready is a broadcast channel over the standard Go close-and-replace
	// idiom: a release closes it to wake every current waiter and installs a
	// fresh one for the next round. A single close can satisfy several waiters
	// at once, so freed bytes are never stranded behind a lone token.
	ready chan struct{}
}

func newMemPool(total int64) *memPool {
	return &memPool{total: total, ready: make(chan struct{})}
}

// reserve waits until n bytes are available, then reserves them. The caller
// must release the same n when it is done. The wait is bounded by ctx.
func (p *memPool) reserve(ctx context.Context, n int64) (release func(), err error) {
	for {
		p.mu.Lock()
		if p.used+n <= p.total {
			p.used += n
			p.mu.Unlock()
			return p.releaseFunc(n), nil
		}
		// Capture this round's channel under the lock: a release swaps in a new
		// one before closing the old, so waiting on the captured channel cannot
		// miss a wakeup that lands between the unlock and the select.
		ready := p.ready
		p.mu.Unlock()

		select {
		case <-ready:
			// A release happened: loop and re-check whether n now fits.
		case <-ctx.Done():
			return nil, fmt.Errorf("waiting for %s of run memory (--max-total-run-memory %s): %w",
				formatBytes(n), formatBytes(p.total), ctx.Err())
		}
	}
}

func (p *memPool) releaseFunc(n int64) func() {
	return func() {
		p.mu.Lock()
		p.used -= n
		// Broadcast by closing the current channel and installing a fresh one:
		// every waiter re-checks against the freed bytes, so one release can
		// admit several runs instead of only the first to wake.
		old := p.ready
		p.ready = make(chan struct{})
		p.mu.Unlock()
		close(old)
	}
}

// formatBytes formats bytes as a human-readable string with SI suffixes.
func formatBytes(b int64) string {
	switch {
	case b >= 1<<30 && b%(1<<30) == 0:
		return fmt.Sprintf("%dGi", b>>30)
	case b >= 1<<20 && b%(1<<20) == 0:
		return fmt.Sprintf("%dMi", b>>20)
	case b >= 1<<10 && b%(1<<10) == 0:
		return fmt.Sprintf("%dKi", b>>10)
	default:
		return fmt.Sprintf("%d", b)
	}
}
