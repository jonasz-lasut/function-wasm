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
	wake chan struct{}
}

func newMemPool(total int64) *memPool {
	return &memPool{total: total, wake: make(chan struct{}, 1)}
}

// reserve waits until n bytes are available, then reserves them. The caller
// must release the same n when it is done. The wait is bounded by ctx.
func (p *memPool) reserve(ctx context.Context, n int64) (release func(), err error) {
	p.mu.Lock()
	// Fast path: fits now.
	if p.used+n <= p.total {
		p.used += n
		p.mu.Unlock()
		return p.releaseFunc(n), nil
	}
	p.mu.Unlock()

	// Slow path: wait for space.
	for {
		select {
		case <-p.wake:
			p.mu.Lock()
			if p.used+n <= p.total {
				p.used += n
				p.mu.Unlock()
				return p.releaseFunc(n), nil
			}
			p.mu.Unlock()
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
		p.mu.Unlock()
		select {
		case p.wake <- struct{}{}:
		default:
		}
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
