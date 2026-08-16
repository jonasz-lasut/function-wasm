package engine

import (
	"sync"
	"time"

	"github.com/jonasz-lasut/function-wasm/internal/cache"
	"github.com/jonasz-lasut/function-wasm/internal/metrics"
)

// DefaultIdleTTL is how long a compiled module stays in memory after its last
// use before the next request for it goes back to the on-disk artifact.
const DefaultIdleTTL = 10 * time.Minute

// CacheOptions configure a Cache.
type CacheOptions struct {
	// Disk is the store of wasmtime artifacts; nil keeps compiled modules in
	// memory only (tests).
	Disk *cache.Store
	// IdleTTL is how long a compiled module stays in memory after its last
	// use; <= 0 means DefaultIdleTTL.
	IdleTTL time.Duration
	// NoMemory disables the memory tier: nothing is retained between
	// requests and every request loads the artifact from Disk (milliseconds)
	// — for runtimes serving large Go modules, whose compiled form is well
	// over 100 MB each, where memory matters more than those milliseconds.
	NoMemory bool
}

// Cache hands out compiled modules by content digest from three tiers: memory
// (hot modules, dropped after IdleTTL idle, or off), the on-disk store of
// wasmtime artifacts (survives restarts, loads in milliseconds), and finally
// fetch + compile (seconds for a large Go guest), whose result is written back
// to disk. A load for one digest runs once even under concurrent requests.
type Cache struct {
	engine   *Engine
	disk     *cache.Store
	ttl      time.Duration
	noMemory bool
	now      func() time.Time

	mu      sync.Mutex
	entries map[string]*entry
	loading map[string]*loading
}

type entry struct {
	module   *Module
	lastUsed time.Time
}

type loading struct {
	done   chan struct{}
	module *Module
	err    error
}

// NewCache returns a Cache over engine.
func NewCache(engine *Engine, o CacheOptions) *Cache {
	if o.IdleTTL <= 0 {
		o.IdleTTL = DefaultIdleTTL
	}
	return &Cache{engine: engine, disk: o.Disk, ttl: o.IdleTTL, noMemory: o.NoMemory, now: time.Now, entries: map[string]*entry{}, loading: map[string]*loading{}}
}

// Get returns the compiled module for digest, calling fetch for its bytes only
// when neither memory nor disk has it. A failed load is not cached.
func (c *Cache) Get(digest string, fetch func() ([]byte, error)) (*Module, error) {
	c.mu.Lock()
	c.expire()
	if e, ok := c.entries[digest]; ok {
		e.lastUsed = c.now()
		c.mu.Unlock()
		metrics.CacheEvents.WithLabelValues(metrics.CacheCompiled, metrics.EventHit).Inc()
		return e.module, nil
	}
	if l, ok := c.loading[digest]; ok {
		c.mu.Unlock()
		<-l.done
		return l.module, l.err
	}
	l := &loading{done: make(chan struct{})}
	c.loading[digest] = l
	c.mu.Unlock()
	metrics.CacheEvents.WithLabelValues(metrics.CacheCompiled, metrics.EventMiss).Inc()

	l.module, l.err = c.load(digest, fetch)

	c.mu.Lock()
	delete(c.loading, digest)
	if l.err == nil && !c.noMemory {
		c.entries[digest] = &entry{module: l.module, lastUsed: c.now()}
	}
	c.mu.Unlock()
	close(l.done)
	return l.module, l.err
}

// load tries the on-disk artifact first, then fetches and compiles, writing
// the new artifact back for the next process. Disk failures never fail the
// request: a full or read-only cache directory only costs a recompile.
func (c *Cache) load(digest string, fetch func() ([]byte, error)) (*Module, error) {
	if c.disk != nil {
		if artifact, ok := c.disk.Get(digest); ok {
			if m, err := c.engine.Deserialize(artifact); err == nil {
				metrics.CacheEvents.WithLabelValues(metrics.CacheCompiledDisk, metrics.EventHit).Inc()
				return m, nil
			}
		}
		metrics.CacheEvents.WithLabelValues(metrics.CacheCompiledDisk, metrics.EventMiss).Inc()
	}
	wasm, err := fetch()
	if err != nil {
		return nil, err
	}
	m, err := c.engine.Compile(wasm)
	if err != nil {
		return nil, err
	}
	if c.disk != nil {
		if artifact, err := c.engine.Serialize(m); err == nil {
			_ = c.disk.Put(digest, artifact)
		}
	}
	return m, nil
}

// expire drops modules idle for longer than the TTL. Called with mu held;
// the map is small (one entry per module in use), so a full walk is cheap.
func (c *Cache) expire() {
	cutoff := c.now().Add(-c.ttl)
	for digest, e := range c.entries {
		if e.lastUsed.Before(cutoff) {
			delete(c.entries, digest)
		}
	}
}

// Len returns the number of modules held in memory.
func (c *Cache) Len() int {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.expire()
	return len(c.entries)
}
