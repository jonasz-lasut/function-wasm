package module

import (
	"sync"
	"time"
)

// maxCachedLayers bounds the manifest→layer cache; past it the map is simply
// reset, which only costs a manifest fetch.
const maxCachedLayers = 1024

// tagCache remembers what a tag resolved to for a TTL, so a Composition
// referencing a moving tag costs one registry round trip per TTL rather than
// one per reconcile.
type tagCache struct {
	ttl time.Duration
	now func() time.Time

	mu      sync.Mutex
	entries map[string]tagEntry
}

type tagEntry struct {
	digest  string
	expires time.Time
}

func newTagCache(ttl time.Duration, now func() time.Time) *tagCache {
	return &tagCache{ttl: ttl, now: now, entries: map[string]tagEntry{}}
}

func (c *tagCache) get(ref string, resolve func() (string, error)) (string, error) {
	c.mu.Lock()
	if e, ok := c.entries[ref]; ok && c.now().Before(e.expires) {
		c.mu.Unlock()
		return e.digest, nil
	}
	c.mu.Unlock()

	digest, err := resolve()
	if err != nil {
		return "", err
	}
	c.mu.Lock()
	c.entries[ref] = tagEntry{digest: digest, expires: c.now().Add(c.ttl)}
	c.mu.Unlock()
	return digest, nil
}

// layerCache remembers which layer of a manifest is the module. Manifests are
// immutable by digest, so entries never expire.
type layerCache struct {
	mu      sync.Mutex
	entries map[string]layerInfo
}

func newLayerCache() *layerCache {
	return &layerCache{entries: map[string]layerInfo{}}
}

func (c *layerCache) get(manifestDigest string, resolve func() (layerInfo, error)) (layerInfo, error) {
	c.mu.Lock()
	if l, ok := c.entries[manifestDigest]; ok {
		c.mu.Unlock()
		return l, nil
	}
	c.mu.Unlock()

	l, err := resolve()
	if err != nil {
		return layerInfo{}, err
	}
	c.mu.Lock()
	if len(c.entries) >= maxCachedLayers {
		c.entries = map[string]layerInfo{}
	}
	c.entries[manifestDigest] = l
	c.mu.Unlock()
	return l, nil
}
