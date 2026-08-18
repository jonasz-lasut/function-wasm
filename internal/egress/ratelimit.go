package egress

import (
	"sync"
	"time"

	"golang.org/x/time/rate"
)

// rateLimiters is a process-wide map of module digest to token bucket,
// with idle expiry so a module that stops being served does not leak.
type rateLimiters struct {
	mu      sync.Mutex
	cfg     rateLimitPolicy
	entries map[string]*rateLimiterEntry
}

type rateLimiterEntry struct {
	limiter  *rate.Limiter
	lastSeen time.Time
}

// idleExpiry is how long an unused entry stays in the map before a
// sweep removes it: twice the token refill period so a module at the
// sustained rate never finds its entry gone.
const idleExpiry = 10 * time.Minute

func newRateLimiters(cfg rateLimitPolicy) *rateLimiters {
	return &rateLimiters{
		cfg:     cfg,
		entries: make(map[string]*rateLimiterEntry),
	}
}

// allow checks whether the module identified by digest may make one
// request right now. It never blocks: a denied request is returned to the
// guest as a budget error, not queued.
func (r *rateLimiters) allow(digest string) bool {
	r.mu.Lock()
	defer r.mu.Unlock()
	e, ok := r.entries[digest]
	if !ok {
		lim := rate.NewLimiter(rate.Limit(r.cfg.requestsPerMinute/60.0), r.cfg.burst)
		e = &rateLimiterEntry{limiter: lim}
		r.entries[digest] = e
	}
	e.lastSeen = time.Now()
	return e.limiter.Allow()
}

// sweep removes entries not seen for idleExpiry.
func (r *rateLimiters) sweep() {
	r.mu.Lock()
	defer r.mu.Unlock()
	cutoff := time.Now().Add(-idleExpiry)
	for k, e := range r.entries {
		if e.lastSeen.Before(cutoff) {
			delete(r.entries, k)
		}
	}
}
