package engine

import (
	"container/list"
	"context"
	"fmt"
	"sync"
)

// fairScheduler is a round-robin slot scheduler: requests with different keys
// are served in turn so one hot module cannot take every slot from every other.
// When the key set has only one entry (one module in use), it degrades to a
// simple FIFO with no overhead beyond the mutex.
type fairScheduler struct {
	mu     sync.Mutex
	total  int // max concurrent slots
	active int // slots currently held
	queues map[string]*list.List
	order  *list.List // round-robin order of keys with pending waiters
}

// waiter is one blocked acquire.
type waiter struct {
	ch  chan struct{} // closed when the waiter is admitted
	key string
}

func newFairScheduler(slots int) *fairScheduler {
	return &fairScheduler{
		total:  slots,
		queues: make(map[string]*list.List),
		order:  list.New(),
	}
}

// acquire waits for a slot. When multiple keys have pending waiters, a freed
// slot is given to the key whose oldest waiter has been waiting the longest
// among keys that have not been served since the last rotation (round-robin
// by key, FIFO within a key).
func (s *fairScheduler) acquire(ctx context.Context, key string) (release func(), err error) {
	s.mu.Lock()

	// Fast path: a slot is free.
	if s.active < s.total {
		s.active++
		s.mu.Unlock()
		return s.releaseFunc(), nil
	}

	// Slow path: enqueue a waiter.
	w := &waiter{ch: make(chan struct{}, 1), key: key}
	q := s.queues[key]
	if q == nil {
		q = list.New()
		s.queues[key] = q
		s.order.PushBack(key)
	}
	q.PushBack(w)
	s.mu.Unlock()

	select {
	case <-w.ch:
		return s.releaseFunc(), nil
	case <-ctx.Done():
		// Remove self from the queue.
		s.mu.Lock()
		s.removeWaiter(key, w)
		s.mu.Unlock()
		return nil, fmt.Errorf("waiting for a run slot: %w", ctx.Err())
	}
}

// releaseFunc returns the function a caller defers after obtaining a slot.
func (s *fairScheduler) releaseFunc() func() {
	return func() {
		s.mu.Lock()
		defer s.mu.Unlock()
		s.active--
		s.wakeNext()
	}
}

// wakeNext admits the next waiter in round-robin order. It must be called
// with mu held.
func (s *fairScheduler) wakeNext() {
	if s.order.Len() == 0 {
		return
	}
	// Walk the round-robin order: the front is the next key to serve.
	// Move it to the back after serving, so every key gets a turn.
	for s.order.Len() > 0 {
		front := s.order.Front()
		key := front.Value.(string) //nolint:errcheck // key is always a string.
		q := s.queues[key]
		if q == nil || q.Len() == 0 {
			// This key has no waiters: remove it and try the next.
			s.order.Remove(front)
			delete(s.queues, key)
			continue
		}
		// Pop the oldest waiter from this key's FIFO.
		elem := q.Front()
		w := q.Remove(elem).(*waiter) //nolint:errcheck // waiter is always *waiter.
		s.active++
		close(w.ch)
		// Rotate: move this key to the back so the next slot goes to
		// a different key.
		s.order.MoveToBack(front)
		// Clean up if this was the last waiter for this key.
		if q.Len() == 0 {
			s.order.Remove(front)
			delete(s.queues, key)
		}
		return
	}
}

// removeWaiter removes w from its key's queue; called when a waiter's context
// is done. Must be called with mu held.
func (s *fairScheduler) removeWaiter(key string, w *waiter) {
	q := s.queues[key]
	if q == nil {
		return
	}
	for e := q.Front(); e != nil; e = e.Next() {
		if e.Value == w {
			q.Remove(e)
			break
		}
	}
	if q.Len() == 0 {
		delete(s.queues, key)
		// Remove from the round-robin order.
		for e := s.order.Front(); e != nil; e = e.Next() {
			if e.Value.(string) == key { //nolint:errcheck // key is always a string.
				s.order.Remove(e)
				break
			}
		}
	}
}
