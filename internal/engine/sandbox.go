package engine

import (
	"fmt"
	"maps"
	"os"
	"slices"

	"github.com/bytecodealliance/wasmtime-go/v47"
	"github.com/crossplane/function-sdk-go/logging"
)

// Sandbox mechanics of one Run (docs/one-pager-sandbox.md, "Filesystem" and
// "Environment"): a WASI pre-open for the private /tmp — the only directory a
// guest is ever given; host directories are deliberately not mountable — and
// WASI environ for the environment. Nothing here touches the ABI — a guest
// uses its language's file and environment APIs.

const (
	// PrivateTmpGuestPath is where a guest sees its private /tmp; Go's
	// os.TempDir and Rust's env::temp_dir default to it on WASI.
	PrivateTmpGuestPath = "/tmp"
	// PrivateTmpPrefix names the per-Run directories under os.TempDir().
	PrivateTmpPrefix = "function-wasm-private-tmp-"
)

// privateTmp creates the Run's private /tmp when opts asks for one and
// returns its host path, or "" when the Run gets none. The caller removes it
// once the store is gone.
func privateTmp(opts RunOptions) (string, error) {
	if !opts.PrivateTmp {
		return "", nil
	}
	dir, err := os.MkdirTemp("", PrivateTmpPrefix)
	if err != nil {
		return "", fmt.Errorf("cannot create the private /tmp: %w", err)
	}
	return dir, nil
}

// removePrivateTmp removes a Run's private /tmp; a failure is logged, not
// returned — the Run's outcome is the guest's, and a leftover directory is
// the operator's to notice.
func removePrivateTmp(dir string, log logging.Logger) {
	if dir == "" {
		return
	}
	if err := os.RemoveAll(dir); err != nil && log != nil {
		log.Info("Cannot remove the private /tmp", "dir", dir, "error", err)
	}
}

// configureSandbox applies the Run's grants to the WASI config: the private
// /tmp (tmpDir, if any) pre-opened writable at /tmp, and the environment.
// With no grants it does nothing, so the default store stays exactly as
// before.
func configureSandbox(wasi *wasmtime.WasiConfig, opts RunOptions, tmpDir string) error {
	if tmpDir != "" {
		if err := wasi.PreopenDir(tmpDir, PrivateTmpGuestPath, wasmtime.DIR_READ|wasmtime.DIR_WRITE, wasmtime.FILE_READ|wasmtime.FILE_WRITE); err != nil {
			return fmt.Errorf("cannot pre-open the private /tmp: %w", err)
		}
	}
	if len(opts.Env) > 0 {
		keys := slices.Sorted(maps.Keys(opts.Env))
		values := make([]string, len(keys))
		for i, k := range keys {
			values[i] = opts.Env[k]
		}
		wasi.SetEnv(keys, values)
	}
	return nil
}
