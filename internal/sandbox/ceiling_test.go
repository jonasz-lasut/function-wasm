package sandbox

import (
	"path/filepath"
	"strings"
	"testing"
)

func TestNewCeiling(t *testing.T) {
	cases := map[string]struct {
		reason string
		opts   Options
	}{
		"NoProbe":    {reason: "A policy that grants no private /tmp needs no probe."},
		"PrivateTmp": {reason: "The private /tmp probe passes under a writable TMPDIR.", opts: Options{ProbePrivateTmp: true}},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			if _, err := NewCeiling(tc.opts); err != nil {
				t.Fatalf("\n%s\nNewCeiling(): unexpected error %v", tc.reason, err)
			}
		})
	}
}

func TestNewCeilingPrivateTmpUnwritable(t *testing.T) {
	t.Setenv("TMPDIR", filepath.Join(t.TempDir(), "missing"))
	_, err := NewCeiling(Options{ProbePrivateTmp: true})
	if err == nil || !strings.Contains(err.Error(), "the operator policy grants a private /tmp (usePrivateTmp), but the runtime cannot create one under") {
		t.Fatalf("NewCeiling(): want the private /tmp probe to fail under an unusable TMPDIR, got %v", err)
	}
}
