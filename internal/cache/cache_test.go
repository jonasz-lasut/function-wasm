package cache

import (
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/spf13/afero"
)

var (
	module = []byte("\x00asm\x01\x00\x00\x00 pretend module")
	digest = digestOf(module)
)

func TestStore(t *testing.T) {
	type want struct {
		got    []byte
		ok     bool
		putErr string
	}
	cases := map[string]struct {
		reason string
		verify bool
		put    []byte
		tamper []byte
		want   want
	}{
		"RoundTrip": {
			reason: "What was put is what Get returns.",
			verify: true,
			put:    module,
			want:   want{got: module, ok: true},
		},
		"VerifyRefusesWrongContent": {
			reason: "A verifying store refuses to file content under a digest it does not have.",
			verify: true,
			put:    []byte("other"),
			want:   want{putErr: "content is sha256:"},
		},
		"VerifyDropsCorruptEntry": {
			reason: "A verifying store removes an entry that no longer matches its digest.",
			verify: true,
			put:    module,
			tamper: []byte("corrupt"),
			want:   want{ok: false},
		},
		"UnverifiedServesAnything": {
			reason: "A non-verifying store (compiled artifacts) trusts its files.",
			verify: false,
			put:    []byte("serialized code"),
			want:   want{got: []byte("serialized code"), ok: true},
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			s := New(afero.NewMemMapFs(), tc.verify)
			err := s.Put(digest, tc.put)
			if tc.want.putErr != "" {
				if err == nil || !contains(err.Error(), tc.want.putErr) {
					t.Fatalf("\n%s\nPut(): want error containing %q, got %v", tc.reason, tc.want.putErr, err)
				}
				return
			}
			if err != nil {
				t.Fatalf("\n%s\nPut(): %v", tc.reason, err)
			}
			if tc.tamper != nil {
				if err := afero.WriteFile(s.fs, fileName(digest), tc.tamper, 0o600); err != nil {
					t.Fatal(err)
				}
			}
			got, ok := s.Get(digest)
			if diff := cmp.Diff(tc.want.ok, ok); diff != "" {
				t.Fatalf("\n%s\nGet() ok: -want, +got:\n%s", tc.reason, diff)
			}
			if diff := cmp.Diff(tc.want.got, got); diff != "" {
				t.Errorf("\n%s\nGet(): -want, +got:\n%s", tc.reason, diff)
			}
			if tc.tamper != nil {
				if _, err := s.fs.Stat(fileName(digest)); err == nil {
					t.Errorf("\n%s\ncorrupt entry was not removed", tc.reason)
				}
			}
		})
	}
}

// TestStoreOnDisk runs the store on the real filesystem, where a base-path
// afero.Fs is less forgiving than MemMapFs about directories that do not
// exist.
func TestStoreOnDisk(t *testing.T) {
	base := afero.NewBasePathFs(afero.NewOsFs(), t.TempDir())
	s, err := Subdir(base, "modules", true)
	if err != nil {
		t.Fatal(err)
	}
	if err := s.Put(digest, module); err != nil {
		t.Fatalf("Put(): %v", err)
	}
	got, ok := s.Get(digest)
	if !ok {
		t.Fatal("Get() after Put() found nothing")
	}
	if diff := cmp.Diff(module, got); diff != "" {
		t.Errorf("Get(): -want, +got:\n%s", diff)
	}
	if got := s.Len(); got != 1 {
		t.Errorf("Len(): want 1, got %d (temp files must not linger)", got)
	}
}

func TestSubdirAndRemoveOthers(t *testing.T) {
	base := afero.NewMemMapFs()
	current, err := Subdir(base, "compiled/v47-arm64", false)
	if err != nil {
		t.Fatal(err)
	}
	old, err := Subdir(base, "compiled/v46-arm64", false)
	if err != nil {
		t.Fatal(err)
	}
	if err := current.Put(digest, []byte("new")); err != nil {
		t.Fatal(err)
	}
	if err := old.Put(digest, []byte("old")); err != nil {
		t.Fatal(err)
	}
	if err := RemoveOthers(base, "compiled", "v47-arm64"); err != nil {
		t.Fatalf("RemoveOthers(): %v", err)
	}
	if _, ok := current.Get(digest); !ok {
		t.Error("the current version's artifact was removed")
	}
	if _, err := base.Stat("compiled/v46-arm64"); err == nil {
		t.Error("the other version's directory survived")
	}
	if err := RemoveOthers(base, "missing", "x"); err != nil {
		t.Errorf("RemoveOthers() on a missing dir: %v", err)
	}
	if got := current.Len(); got != 1 {
		t.Errorf("Len(): want 1, got %d", got)
	}
}

func contains(s, sub string) bool {
	return len(sub) == 0 || (len(s) >= len(sub) && index(s, sub) >= 0)
}

func index(s, sub string) int {
	for i := 0; i+len(sub) <= len(s); i++ {
		if s[i:i+len(sub)] == sub {
			return i
		}
	}
	return -1
}
