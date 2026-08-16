package cache

import (
	"strings"
	"testing"
	"time"

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
				if err == nil || !strings.Contains(err.Error(), tc.want.putErr) {
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
	// Written just now: another version's directory is in use by a pod of
	// that version (a rolling upgrade shares the volume) and stays.
	if err := RemoveOthers(base, "compiled", "v47-arm64", time.Now()); err != nil {
		t.Fatalf("RemoveOthers(): %v", err)
	}
	if _, err := base.Stat("compiled/v46-arm64"); err != nil {
		t.Error("the other version's directory was removed while its artifacts were fresh")
	}
	// Untouched for longer than StaleVersionAge: gone.
	if err := RemoveOthers(base, "compiled", "v47-arm64", time.Now().Add(StaleVersionAge+time.Minute)); err != nil {
		t.Fatalf("RemoveOthers(): %v", err)
	}
	if _, ok := current.Get(digest); !ok {
		t.Error("the current version's artifact was removed")
	}
	if _, err := base.Stat("compiled/v46-arm64"); err == nil {
		t.Error("the other version's stale directory survived")
	}
	if err := RemoveOthers(base, "missing", "x", time.Now()); err != nil {
		t.Errorf("RemoveOthers() on a missing dir: %v", err)
	}
	if got := current.Len(); got != 1 {
		t.Errorf("Len(): want 1, got %d", got)
	}
}

func TestSweep(t *testing.T) {
	blobs := New(afero.NewMemMapFs(), false)
	compiled := New(afero.NewMemMapFs(), false)
	base := time.Unix(1_700_000_000, 0)
	// Four entries of 100 bytes, used at one-hour intervals: the oldest two
	// go when the bound is 250 bytes (down to 90 % of it, 225 → 200 left).
	for i, s := range []struct {
		store  *Store
		digest string
	}{
		{blobs, "sha256:aaaa"},
		{compiled, "sha256:bbbb"},
		{blobs, "sha256:cccc"},
		{compiled, "sha256:dddd"},
	} {
		if err := s.store.Put(s.digest, make([]byte, 100)); err != nil {
			t.Fatal(err)
		}
		used := base.Add(time.Duration(i) * time.Hour)
		if err := s.store.fs.Chtimes(fileName(s.digest), used, used); err != nil {
			t.Fatal(err)
		}
	}
	freed, err := Sweep([]*Store{blobs, compiled}, 250)
	if err != nil {
		t.Fatalf("Sweep(): %v", err)
	}
	if freed != 200 {
		t.Errorf("Sweep() freed %d bytes, want 200", freed)
	}
	if _, ok := blobs.Get("sha256:aaaa"); ok {
		t.Error("the least recently used entry survived")
	}
	if _, ok := compiled.Get("sha256:bbbb"); ok {
		t.Error("the second least recently used entry survived")
	}
	if _, ok := blobs.Get("sha256:cccc"); !ok {
		t.Error("a recent entry was removed")
	}
	if got := blobs.Bytes() + compiled.Bytes(); got != 200 {
		t.Errorf("Bytes(): want 200 left, got %d", got)
	}
	// Under the bound nothing moves; a bound of zero is off.
	for _, max := range []int64{1000, 0} {
		if freed, err := Sweep([]*Store{blobs, compiled}, max); err != nil || freed != 0 {
			t.Errorf("Sweep(max=%d): freed %d, err %v; want nothing", max, freed, err)
		}
	}
}

func TestGetTouches(t *testing.T) {
	s := New(afero.NewMemMapFs(), true)
	if err := s.Put(digest, module); err != nil {
		t.Fatal(err)
	}
	old := time.Unix(1_700_000_000, 0)
	if err := s.fs.Chtimes(fileName(digest), old, old); err != nil {
		t.Fatal(err)
	}
	if _, ok := s.Get(digest); !ok {
		t.Fatal("Get(): miss")
	}
	entries, err := s.Entries()
	if err != nil {
		t.Fatal(err)
	}
	if len(entries) != 1 || !entries[0].LastUsed.After(old) {
		t.Errorf("a read should count as a use for the sweep, got %+v", entries)
	}
}
