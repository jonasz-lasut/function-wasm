package engine

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"google.golang.org/protobuf/testing/protocmp"

	fnv1 "github.com/crossplane/function-sdk-go/proto/v1"

	"github.com/jonasz-lasut/function-wasm/internal/testwasm"
)

// resultOf is what the testwasm WASI fixtures return: the bytes they obtained
// as the message of a normal result.
func resultOf(msg string) *fnv1.RunFunctionResponse {
	return &fnv1.RunFunctionResponse{Results: []*fnv1.Result{{Severity: fnv1.Severity_SEVERITY_NORMAL, Message: msg}}}
}

// TestRunSandbox pins the sandbox mechanics through raw WASI calls: the
// private /tmp is writable, empty per run and refuses a path escape, the
// environment is exactly the grant — and, without a grant, the store is the
// default one: no pre-opens, no environment.
func TestRunSandbox(t *testing.T) {
	type args struct {
		run  RunOptions
		opts testwasm.Options
	}
	type want struct {
		rsp *fnv1.RunFunctionResponse
		err string
	}
	cases := map[string]struct {
		reason string
		args   args
		want   want
	}{
		"PrivateTmpWriteRead": {
			reason: "The private /tmp is writable, and a write is visible within the run.",
			args:   args{run: RunOptions{PrivateTmp: true}, opts: testwasm.WriteRead("scratch.txt", "written by the guest")},
			want:   want{rsp: resultOf("written by the guest")},
		},
		"PrivateTmpEscapeRefused": {
			reason: "A path leaving the private /tmp is refused by wasmtime (EPERM, 63): the pre-open is the only directory the guest has and it cannot climb out of it.",
			args:   args{run: RunOptions{PrivateTmp: true}, opts: testwasm.ReadFile("../secret.txt")},
			want:   want{err: "wasmfn_run failed: module exited with status 63"},
		},
		"PrivateTmpEmpty": {
			reason: "Every run gets an empty private /tmp: a file another run wrote is not there (ENOENT, 44).",
			args:   args{run: RunOptions{PrivateTmp: true}, opts: testwasm.ReadFile("scratch.txt")},
			want:   want{err: "wasmfn_run failed: module exited with status 44"},
		},
		"Env": {
			reason: "The environment is exactly the grant, in key order.",
			args:   args{run: RunOptions{Env: map[string]string{"GREETING": "hello", "A": "b"}}, opts: testwasm.Environ()},
			want:   want{rsp: resultOf("A=b\x00GREETING=hello\x00")},
		},
		"NoPreopensByDefault": {
			reason: "Without a grant there is no pre-opened directory: descriptor 3 does not exist (EBADF, 8).",
			args:   args{opts: testwasm.ReadFile("hello.txt")},
			want:   want{err: "wasmfn_run failed: module exited with status 8"},
		},
		"NoEnvByDefault": {
			reason: "Without a grant the environment is empty; the host's is never inherited.",
			args:   args{opts: testwasm.Environ()},
			want:   want{rsp: resultOf("")},
		},
	}

	e, err := New(Config{})
	if err != nil {
		t.Fatalf("New(): %v", err)
	}
	defer e.Close()

	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			m, err := e.Compile(testwasm.Fixed(t, cannedResponse(), tc.args.opts))
			if err != nil {
				t.Fatalf("Compile(): %v", err)
			}
			defer m.Release()

			got, err := e.Run(context.Background(), m, request(), &recorder{}, tc.args.run)

			if tc.want.err != "" {
				if err == nil || !strings.Contains(err.Error(), tc.want.err) {
					t.Fatalf("\n%s\nRun(): want error containing %q, got %v", tc.reason, tc.want.err, err)
				}
			} else if err != nil {
				t.Fatalf("\n%s\nRun(): unexpected error: %v", tc.reason, err)
			}
			if diff := cmp.Diff(tc.want.rsp, got, protocmp.Transform()); diff != "" {
				t.Errorf("\n%s\nRun(): -want, +got:\n%s", tc.reason, diff)
			}
		})
	}
}

// TestRunPrivateTmpRemoved pins that a run's private /tmp is gone once the
// run is over — after a clean run, a trap and a WASI exit alike — and that
// it lives under os.TempDir(), which is what an operator points at a bounded
// tmpfs.
func TestRunPrivateTmpRemoved(t *testing.T) {
	tmp := t.TempDir()
	t.Setenv("TMPDIR", tmp)

	dir, err := privateTmp(RunOptions{PrivateTmp: true})
	if err != nil {
		t.Fatalf("privateTmp(): %v", err)
	}
	if filepath.Dir(dir) != tmp {
		t.Errorf("privateTmp() = %s, want a directory under $TMPDIR %s", dir, tmp)
	}
	removePrivateTmp(dir, nil)
	if _, err := os.Stat(dir); !os.IsNotExist(err) {
		t.Errorf("removePrivateTmp(): %s still exists (%v)", dir, err)
	}

	e, err := New(Config{})
	if err != nil {
		t.Fatalf("New(): %v", err)
	}
	defer e.Close()

	cases := map[string]struct {
		reason string
		opts   testwasm.Options
	}{
		"Clean": {reason: "A run that wrote to /tmp leaves nothing behind.", opts: testwasm.WriteRead("scratch.txt", "written by the guest")},
		"Trap":  {reason: "A trapping run leaves nothing behind.", opts: testwasm.Options{Body: "unreachable"}},
		"Exit":  {reason: "A run that exits through WASI leaves nothing behind.", opts: testwasm.ReadFile("missing.txt")},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			m, err := e.Compile(testwasm.Fixed(t, cannedResponse(), tc.opts))
			if err != nil {
				t.Fatalf("Compile(): %v", err)
			}
			defer m.Release()
			_, _ = e.Run(context.Background(), m, request(), &recorder{}, RunOptions{PrivateTmp: true})

			entries, err := os.ReadDir(tmp)
			if err != nil {
				t.Fatal(err)
			}
			if len(entries) != 0 {
				names := make([]string, len(entries))
				for i, e := range entries {
					names[i] = e.Name()
				}
				t.Errorf("\n%s\nprivate /tmp left behind under %s: %v", tc.reason, tmp, names)
			}
		})
	}
}
