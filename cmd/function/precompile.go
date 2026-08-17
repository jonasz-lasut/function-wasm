package main

import (
	"context"
	"fmt"
	"runtime/debug"
	"sync"
	"sync/atomic"
	"time"

	"github.com/crossplane/function-sdk-go"
	"github.com/crossplane/function-sdk-go/errors"

	"github.com/jonasz-lasut/function-wasm/internal/engine"
)

// PrecompileCmd compiles modules ahead of time into the shared cache volume
// so the serving pods map the artifacts on startup, paying no compile cost.
// It runs the same resolve -> verify -> load -> release path as --warm-modules
// but exits non-zero when any entry fails: the operator's init container or
// Job should surface the error, not silently serve cold.
type PrecompileCmd struct {
	CeilingFlags `embed:""`

	MaxConcurrentCompiles int      `help:"Most modules compiled at once." default:"1" env:"MAX_CONCURRENT_COMPILES"`
	Modules               []string `arg:"" required:"" help:"Modules to precompile: OCI references pinned to their manifest digest (repo[:tag]@sha256:...) and, with --module-dir, path:<file> entries."`
}

// Run precompiles the modules and exits non-zero on any failure.
func (c *PrecompileCmd) Run(cli *CLI) error {
	log, err := function.NewLogger(cli.Debug)
	if err != nil {
		return err
	}

	cfg := c.engineConfig()
	eng, err := engine.New(cfg)
	if err != nil {
		return err
	}
	defer eng.Close()

	blobs, compiled, manifests, err := openCaches(c.EnableFuel)
	if err != nil {
		return err
	}

	resolver, err := c.resolver(blobs)
	if err != nil {
		return err
	}

	fn := &Function{
		log:    log,
		engine: eng,
		modules: engine.NewCache(eng, engine.CacheOptions{
			Disk:                  compiled,
			MaxConcurrentCompiles: c.MaxConcurrentCompiles,
		}),
		resolver:  resolver,
		manifests: manifests,
	}

	return fn.precompile(context.Background(), c.Modules, c.MaxConcurrentCompiles)
}

// precompile loads the named modules the way a request would - resolve,
// verify, then the cache: memory, the artifact on disk, or fetch and
// compile - at most parallel at once, and returns an error when any entry
// fails. Unlike warm, a failure stops the process: the operator's init
// container or Job must surface it.
func (f *Function) precompile(ctx context.Context, entries []string, parallel int) error {
	if len(entries) == 0 {
		return errors.New("no modules to precompile")
	}
	if parallel <= 0 {
		parallel = 1
	}
	start := time.Now()
	f.log.Info("Precompiling modules", "count", len(entries))
	var wg sync.WaitGroup
	var failed atomic.Int64
	slots := make(chan struct{}, parallel)
	for _, entry := range entries {
		slots <- struct{}{}
		wg.Add(1)
		go func() {
			defer wg.Done()
			defer func() { <-slots }()
			defer func() {
				if r := recover(); r != nil {
					failed.Add(1)
					f.log.Info("Panic while precompiling module", "module", entry, "panic", fmt.Sprint(r), "stack", string(debug.Stack()))
				}
			}()
			if err := f.warmOne(ctx, entry); err != nil {
				failed.Add(1)
				f.log.Info("Cannot precompile module", "module", entry, "error", err)
			}
		}()
	}
	wg.Wait()
	n := failed.Load()
	f.log.Info("Precompiled modules", "loaded", int64(len(entries))-n, "failed", n, "duration", time.Since(start).String())
	if n > 0 {
		return exitError{code: 1}
	}
	return nil
}
