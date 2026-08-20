package testwasm

import (
	"context"
	"os"
	"os/exec"
	"path/filepath"
	"sync"
	"testing"
)

type guestBuild struct {
	once sync.Once
	wasm []byte
	err  error
}

var guestBuilds sync.Map // map[string]*guestBuild

// BuildGuest compiles the Go guest module in dir to a wasip1 reactor with the
// Go toolchain on PATH and returns its bytes. The build runs once per process
// per directory. It is skipped in -short mode: a real guest takes several
// seconds to build.
func BuildGuest(t *testing.T, dir string) []byte {
	t.Helper()
	return build(t, "go", dir, func(out string) *exec.Cmd {
		cmd := exec.CommandContext(context.Background(), "go", "build", "-buildmode=c-shared", "-trimpath", "-ldflags=-s -w", "-o", out, ".") //nolint:gosec // Test helper building a checked-in guest.
		cmd.Env = append(os.Environ(), "GOOS=wasip1", "GOARCH=wasm")
		return cmd
	})
}

// BuildTinyGoGuest compiles the guest in dir with TinyGo, skipping the test
// when tinygo is not on PATH.
func BuildTinyGoGuest(t *testing.T, dir string) []byte {
	t.Helper()
	if _, err := exec.LookPath("tinygo"); err != nil {
		t.Skip("tinygo not on PATH")
	}
	return build(t, "tinygo", dir, func(out string) *exec.Cmd {
		return exec.CommandContext(context.Background(), "tinygo", "build", "-target=wasip1", "-buildmode=c-shared", "-no-debug", "-o", out, ".") //nolint:gosec // Test helper building a checked-in guest.
	})
}

// BuildRustGuest compiles the Cargo crate in dir for wasm32-wasip1 (release
// profile), skipping the test when cargo or the target is missing.
func BuildRustGuest(t *testing.T, dir string) []byte {
	t.Helper()
	if _, err := exec.LookPath("cargo"); err != nil {
		t.Skip("cargo not on PATH")
	}
	if out, err := exec.CommandContext(context.Background(), "rustup", "target", "list", "--installed").Output(); err != nil || !containsLine(string(out), "wasm32-wasip1") {
		t.Skip("rustup target wasm32-wasip1 not installed")
	}
	return build(t, "cargo", dir, func(out string) *exec.Cmd {
		// cargo has no -o: build into a per-test target dir and copy.
		target := filepath.Join(filepath.Dir(out), "target")
		script := "cargo build --release --target wasm32-wasip1 --target-dir " + shellQuote(target) +
			" && cp " + shellQuote(filepath.Join(target, "wasm32-wasip1", "release")) + "/*.wasm " + shellQuote(out)
		return exec.CommandContext(context.Background(), "sh", "-c", script) //nolint:gosec // Test helper building a checked-in guest.
	})
}

// BuildZigGuest compiles the Zig project in dir to a wasip1 reactor with the
// zig toolchain (which also builds its vendored fnv1 codec), skipping the test
// when zig is not on PATH.
func BuildZigGuest(t *testing.T, dir string) []byte {
	t.Helper()
	return buildWithZig(t, dir)
}

// BuildCGuest compiles the C project in dir to a wasip1 reactor: a C guest
// builds with zig too (zig cc behind its build.zig, which also compiles the
// nanopb codec), so it is skipped when zig is not on PATH.
func BuildCGuest(t *testing.T, dir string) []byte {
	t.Helper()
	return buildWithZig(t, dir)
}

// BuildASGuest compiles the AssemblyScript project in dir with asc (through
// npm ci + npm run build), skipping the test when npm is not on PATH.
func BuildASGuest(t *testing.T, dir string) []byte {
	t.Helper()
	if _, err := exec.LookPath("npm"); err != nil {
		t.Skip("npm not on PATH")
	}
	return build(t, "npm", dir, func(out string) *exec.Cmd {
		// asc writes the target's outFile inside the project; build there and copy.
		script := "npm ci --no-audit --no-fund && npm run build && cp fn.wasm " + shellQuote(out)
		return exec.CommandContext(context.Background(), "sh", "-c", script) //nolint:gosec // Test helper building a checked-in guest.
	})
}

func buildWithZig(t *testing.T, dir string) []byte {
	t.Helper()
	if _, err := exec.LookPath("zig"); err != nil {
		t.Skip("zig not on PATH")
	}
	return build(t, "zig", dir, func(out string) *exec.Cmd {
		// zig build installs to a prefix; build into a per-test one and copy.
		prefix := filepath.Join(filepath.Dir(out), "zig-prefix")
		script := "zig build -Doptimize=ReleaseSmall --prefix " + shellQuote(prefix) +
			" && cp " + shellQuote(filepath.Join(prefix, "bin", "fn.wasm")) + " " + shellQuote(out)
		return exec.CommandContext(context.Background(), "sh", "-c", script) //nolint:gosec // Test helper building a checked-in guest.
	})
}

func build(t *testing.T, tool, dir string, command func(out string) *exec.Cmd) []byte {
	t.Helper()
	if testing.Short() {
		t.Skip("skipping guest build in -short mode")
	}
	abs, err := filepath.Abs(dir)
	if err != nil {
		t.Fatal(err)
	}
	v, _ := guestBuilds.LoadOrStore(tool+":"+abs, &guestBuild{})
	b := v.(*guestBuild)
	b.once.Do(func() {
		out := filepath.Join(t.TempDir(), "guest.wasm")
		cmd := command(out)
		cmd.Dir = abs
		if output, err := cmd.CombinedOutput(); err != nil {
			b.err = &buildError{dir: abs, output: string(output), err: err}
			return
		}
		b.wasm, b.err = os.ReadFile(out) //nolint:gosec // out is the temp file written just above.
	})
	if b.err != nil {
		t.Fatalf("cannot build guest: %v", b.err)
	}
	return b.wasm
}

func containsLine(s, line string) bool {
	for _, l := range splitLines(s) {
		if l == line {
			return true
		}
	}
	return false
}

func splitLines(s string) []string {
	var lines []string
	start := 0
	for i := 0; i < len(s); i++ {
		if s[i] == '\n' {
			lines = append(lines, s[start:i])
			start = i + 1
		}
	}
	if start < len(s) {
		lines = append(lines, s[start:])
	}
	return lines
}

func shellQuote(s string) string {
	return "'" + s + "'"
}

type buildError struct {
	dir    string
	output string
	err    error
}

func (e *buildError) Error() string {
	return "build in " + e.dir + ": " + e.err.Error() + "\n" + e.output
}
