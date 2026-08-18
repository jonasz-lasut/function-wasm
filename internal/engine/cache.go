package engine

import (
	"context"
	"fmt"
	"sync"
	"time"

	"github.com/jonasz-lasut/function-wasm/internal/cache"
	"github.com/jonasz-lasut/function-wasm/internal/metrics"
)

// Defaults applied for zero CacheOptions fields.
const (
	// DefaultIdleTTL is how long a compiled module stays in memory after its
	// last use before the next request for it goes back to the on-disk
	// artifact.
	DefaultIdleTTL = 10 * time.Minute
	// DefaultLoadTimeout bounds one fetch + compile. It is generous: a large
	// Go guest downloads in seconds and compiles in tens of CPU-seconds, and
	// a load that gives up early only costs the next request the same again.
	DefaultLoadTimeout = 10 * time.Minute
)

// CacheOptions configure a Cache.
type CacheOptions struct {
	// Disk is the store of wasmtime artifacts; nil keeps compiled modules in
	// memory only (tests).
	Disk *cache.Store
	// IdleTTL is how long a compiled module stays in memory after its last
	// use; <= 0 means DefaultIdleTTL.
	IdleTTL time.Duration
	// NoMemory disables the memory tier: nothing is retained between
	// requests and every request maps the artifact from Disk (milliseconds)
	// and releases it afterwards — for runtimes that would rather pay those
	// milliseconds than keep large modules resident.
	NoMemory bool
	// MaxEntries bounds the memory tier: beyond it the least recently used
	// module is dropped (and freed once no run holds it). <= 0 means no
	// bound — the idle TTL alone decides.
	MaxEntries int
	// MaxConcurrentCompiles bounds how many modules compile at once. A
	// compile uses every core wasmtime is given and roughly a gigabyte for
	// a large Go guest, so more than one at a time only multiplies memory;
	// further loads wait their turn, and their requesters with them. <= 0
	// means 1.
	MaxConcurrentCompiles int
	// LoadTimeout bounds one fetch + compile, run under its own context: the
	// request that triggered the load may be cancelled without poisoning
	// the load every waiting request shares. <= 0 means DefaultLoadTimeout.
	LoadTimeout time.Duration
}

// Cache hands out compiled modules by content digest from three tiers: memory
// (hot modules, dropped after IdleTTL idle or past MaxEntries, or off), the
// on-disk store of wasmtime artifacts (survives restarts, maps in
// milliseconds), and finally fetch + compile (seconds for a large Go guest),
// whose result is written back to disk. A load for one digest runs once even
// under concurrent requests, and at most MaxConcurrentCompiles compile at a
// time.
//
// A module handed out by Get is leased: the caller must Release it when its
// run is over, so a module dropped from the tier is freed exactly when its
// last run finishes.
type Cache struct {
	engine      *Engine
	disk        *cache.Store
	ttl         time.Duration
	noMemory    bool
	maxEntries  int
	loadTimeout time.Duration
	compiles    chan struct{}
	now         func() time.Time

	mu      sync.Mutex
	entries map[string]*entry
	loading map[string]*loading
}

type entry struct {
	module   *Module
	lastUsed time.Time
}

type loading struct {
	done    chan struct{}
	waiters int
	module  *Module
	err     error
}

// NewCache returns a Cache over engine.
func NewCache(engine *Engine, o CacheOptions) *Cache {
	if o.IdleTTL <= 0 {
		o.IdleTTL = DefaultIdleTTL
	}
	if o.MaxConcurrentCompiles <= 0 {
		o.MaxConcurrentCompiles = 1
	}
	if o.LoadTimeout <= 0 {
		o.LoadTimeout = DefaultLoadTimeout
	}
	return &Cache{
		engine:      engine,
		disk:        o.Disk,
		ttl:         o.IdleTTL,
		noMemory:    o.NoMemory,
		maxEntries:  o.MaxEntries,
		loadTimeout: o.LoadTimeout,
		compiles:    make(chan struct{}, o.MaxConcurrentCompiles),
		now:         time.Now,
		entries:     map[string]*entry{},
		loading:     map[string]*loading{},
	}
}

// Get returns the compiled module for digest, calling fetch for its bytes only
// when neither memory nor disk has it. The module is leased to the caller,
// who must Release it after use. A failed load is not cached. ctx bounds the
// caller's wait only; the load itself runs under the cache's own timeout.
func (c *Cache) Get(ctx context.Context, digest string, fetch func(ctx context.Context) ([]byte, error)) (*Module, error) {
	c.mu.Lock()
	c.expire()
	if e, ok := c.entries[digest]; ok {
		e.lastUsed = c.now()
		e.module.acquire()
		c.mu.Unlock()
		metrics.CacheEvents.WithLabelValues(metrics.CacheCompiled, metrics.EventHit).Inc()
		return e.module, nil
	}
	if l, ok := c.loading[digest]; ok {
		l.waiters++
		c.mu.Unlock()
		select {
		case <-l.done:
			return l.module, l.err
		case <-ctx.Done():
			// The lease this waiter was promised is returned.
			go func() {
				<-l.done
				if l.module != nil {
					l.module.Release()
				}
			}()
			return nil, ctx.Err()
		}
	}
	l := &loading{done: make(chan struct{})}
	c.loading[digest] = l
	c.mu.Unlock()
	metrics.CacheEvents.WithLabelValues(metrics.CacheCompiled, metrics.EventMiss).Inc()

	// The bookkeeping runs on the way out even if the load panics: a
	// digest must never be left loading forever.
	defer func() {
		c.mu.Lock()
		delete(c.loading, digest)
		if l.module != nil {
			// One lease per waiter on top of the leader's own; the memory
			// tier takes one too when it keeps the module.
			for range l.waiters {
				l.module.acquire()
			}
			if !c.noMemory {
				l.module.acquire()
				c.entries[digest] = &entry{module: l.module, lastUsed: c.now()}
				c.evict()
			}
		}
		c.mu.Unlock()
		close(l.done)
	}()

	loadCtx, cancel := context.WithTimeout(context.WithoutCancel(ctx), c.loadTimeout)
	defer cancel()
	l.module, l.err = c.load(loadCtx, digest, fetch)
	return l.module, l.err
}

// load tries the on-disk artifact first, then fetches and compiles, writing
// the new artifact back for the next process. Disk failures never fail the
// request: a full or read-only cache directory only costs a recompile.
func (c *Cache) load(ctx context.Context, digest string, fetch func(ctx context.Context) ([]byte, error)) (*Module, error) {
	if c.disk != nil {
		if m, ok := c.fromDisk(digest); ok {
			metrics.CacheEvents.WithLabelValues(metrics.CacheCompiledDisk, metrics.EventHit).Inc()
			return m, nil
		}
		metrics.CacheEvents.WithLabelValues(metrics.CacheCompiledDisk, metrics.EventMiss).Inc()
	}
	wasm, err := fetch(ctx)
	if err != nil {
		return nil, err
	}
	select {
	case c.compiles <- struct{}{}:
		defer func() { <-c.compiles }()
	case <-ctx.Done():
		return nil, fmt.Errorf("timed out waiting for a compile slot: %w", ctx.Err())
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

// fromDisk loads the artifact for digest: mapped from its file when the store
// is on a real filesystem — the artifact's pages are then file-backed and
// shared with the page cache rather than copied into the Go heap — or from
// the bytes the store returns. An artifact wasmtime refuses (another version,
// a corrupt file) is a miss.
func (c *Cache) fromDisk(digest string) (*Module, bool) {
	if p, ok := c.disk.Path(digest); ok {
		if m, err := c.engine.DeserializeFile(p); err == nil {
			return m, true
		}
		metrics.CacheEvents.WithLabelValues(metrics.CacheCompiledDisk, metrics.EventStale).Inc()
		return nil, false
	}
	artifact, ok := c.disk.Get(digest)
	if !ok {
		return nil, false
	}
	m, err := c.engine.Deserialize(artifact)
	if err != nil {
		metrics.CacheEvents.WithLabelValues(metrics.CacheCompiledDisk, metrics.EventStale).Inc()
		return nil, false
	}
	return m, true
}

// expire drops modules idle for longer than the TTL. Called with mu held;
// the map is small (one entry per module in use), so a full walk is cheap.
func (c *Cache) expire() {
	cutoff := c.now().Add(-c.ttl)
	for digest, e := range c.entries {
		if e.lastUsed.Before(cutoff) {
			c.drop(digest, e)
		}
	}
}

// evict drops least recently used modules while the tier is over its bound.
// Called with mu held.
func (c *Cache) evict() {
	for c.maxEntries > 0 && len(c.entries) > c.maxEntries {
		var oldest string
		var oldestEntry *entry
		for digest, e := range c.entries {
			if oldestEntry == nil || e.lastUsed.Before(oldestEntry.lastUsed) {
				oldest, oldestEntry = digest, e
			}
		}
		c.drop(oldest, oldestEntry)
	}
}

// drop removes an entry and returns the tier's lease on its module, freeing
// it once no run holds it. Called with mu held.
func (c *Cache) drop(digest string, e *entry) {
	delete(c.entries, digest)
	e.module.Release()
}

// Len returns the number of modules held in memory.
func (c *Cache) Len() int {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.expire()
	return len(c.entries)
}
