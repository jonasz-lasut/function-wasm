package engine

import (
	"container/list"
	"sync"

	"github.com/jonasz-lasut/function-wasm/internal/metrics"
)

// Cache keeps the most recently used compiled modules by content digest.
// Compiling a large Go module costs seconds, so one Function process keeps
// the modules its Compositions use hot; a fetch-and-compile for one digest
// runs once even under concurrent requests.
type Cache struct {
	size int

	mu      sync.Mutex
	entries map[string]*list.Element
	order   *list.List
	loading map[string]*loading
}

type entry struct {
	digest string
	module *Module
}

type loading struct {
	done   chan struct{}
	module *Module
	err    error
}

// NewCache returns a Cache holding up to size modules; size < 1 means 1.
func NewCache(size int) *Cache {
	if size < 1 {
		size = 1
	}
	return &Cache{size: size, entries: map[string]*list.Element{}, order: list.New(), loading: map[string]*loading{}}
}

// Get returns the module for digest, calling load to produce it on a miss.
// Concurrent callers for the same digest share one load; a failed load is not
// cached.
func (c *Cache) Get(digest string, load func() (*Module, error)) (*Module, error) {
	c.mu.Lock()
	if el, ok := c.entries[digest]; ok {
		c.order.MoveToFront(el)
		c.mu.Unlock()
		metrics.CacheEvents.WithLabelValues(metrics.CacheCompiled, metrics.EventHit).Inc()
		return el.Value.(*entry).module, nil
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

	l.module, l.err = load()

	c.mu.Lock()
	delete(c.loading, digest)
	if l.err == nil {
		c.entries[digest] = c.order.PushFront(&entry{digest: digest, module: l.module})
		for c.order.Len() > c.size {
			last := c.order.Back()
			c.order.Remove(last)
			delete(c.entries, last.Value.(*entry).digest)
		}
	}
	c.mu.Unlock()
	close(l.done)
	return l.module, l.err
}

// Len returns the number of cached modules.
func (c *Cache) Len() int {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.order.Len()
}
