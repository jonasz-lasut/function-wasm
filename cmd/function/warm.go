package main

import (
	"context"
	"fmt"
	"runtime/debug"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/crossplane/function-sdk-go/errors"

	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
)

// warmPathPrefix marks a --warm-modules entry as a file under --module-dir;
// every other entry is an OCI reference.
const warmPathPrefix = "path:"

// warmSource turns one --warm-modules entry into the module source a
// Composition would name: path:<file> is a Path source under --module-dir,
// anything else an OCI reference — pinned to its manifest digest like every
// other, the resolver refuses a bare tag.
func warmSource(entry string) v1beta1.ModuleSource {
	if p, ok := strings.CutPrefix(entry, warmPathPrefix); ok {
		return v1beta1.ModuleSource{Type: v1beta1.ModuleTypePath, Path: p}
	}
	return v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, OCI: &v1beta1.OCISource{Ref: entry}}
}

// warm loads the modules named by entries the way a request would — resolve,
// verify, then the cache: memory, the artifact on disk, or fetch and compile
// — at most parallel at once (the compile slots: more would only queue on
// them holding fetched bytes), and returns once every entry is loaded or has
// failed. A failure is logged with its entry and nothing more: a wrong entry
// or an unreachable registry must not keep the pod from serving every other
// module, and the module it names is loaded on its first request as usual.
func (f *Function) warm(ctx context.Context, entries []string, parallel int) {
	if len(entries) == 0 {
		return
	}
	if parallel <= 0 {
		parallel = 1
	}
	start := time.Now()
	f.log.Info("Warming modules", "count", len(entries))
	var wg sync.WaitGroup
	var failed atomic.Int64
	slots := make(chan struct{}, parallel)
	for _, entry := range entries {
		slots <- struct{}{}
		wg.Add(1)
		go func() {
			defer wg.Done()
			defer func() { <-slots }()
			// A bug on the load path is a fatal result for a request; at
			// startup it must not be a crash loop either.
			defer func() {
				if r := recover(); r != nil {
					failed.Add(1)
					f.log.Info("Panic while warming module", "module", entry, "panic", fmt.Sprint(r), "stack", string(debug.Stack()))
				}
			}()
			if err := f.warmOne(ctx, entry); err != nil {
				failed.Add(1)
				f.log.Info("Cannot warm module", "module", entry, "error", err)
			}
		}()
	}
	wg.Wait()
	f.log.Info("Warmed modules", "loaded", int64(len(entries))-failed.Load(), "failed", failed.Load(), "duration", time.Since(start).String())
}

// warmOne loads one entry and returns its lease at once: the memory tier
// keeps its own, and with the tier off the artifact on disk is the point.
func (f *Function) warmOne(ctx context.Context, entry string) error {
	ref, err := f.resolver.Resolve(ctx, warmSource(entry), nil)
	if err != nil {
		return errors.Wrap(err, "cannot resolve module")
	}
	if err := ref.Verify(ctx); err != nil {
		return errors.Wrapf(err, "cannot verify module %s", ref.Description)
	}
	log := f.log.WithValues("module", ref.Description, "digest", ref.Digest)
	mod, err := f.load(ctx, ref, log)
	if err != nil {
		return err
	}
	defer mod.Release()
	// A warmed module's manifest is read and parsed now too, so its first
	// request pays neither; what it requires is worth a line at debug.
	m, err := f.manifestFor(ctx, ref)
	if err != nil {
		return err
	}
	if m != nil {
		log.Debug("Warmed module has a manifest", "manifest", m.Summary())
	}
	return nil
}
