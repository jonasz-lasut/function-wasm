package engine

import (
	"context"
	"sync"
	"testing"
	"time"
)

func TestMemPool(t *testing.T) {
	cases := map[string]struct {
		reason string
		total  int64
		allocs []int64
		want   int // max concurrent
	}{
		"OneAtATime": {
			reason: "A pool of 512 Mi with two 512 Mi reservations serializes them.",
			total:  512 << 20,
			allocs: []int64{512 << 20, 512 << 20},
			want:   1,
		},
		"TwoFit": {
			reason: "A pool of 1 Gi fits two 512 Mi reservations at once.",
			total:  1 << 30,
			allocs: []int64{512 << 20, 512 << 20},
			want:   2,
		},
		"MixedSizes": {
			reason: "A 768 Mi pool fits one 512 Mi and one 256 Mi at once.",
			total:  768 << 20,
			allocs: []int64{512 << 20, 256 << 20},
			want:   2,
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			p := newMemPool(tc.total)
			ctx := context.Background()
			var wg sync.WaitGroup
			maxC := make(chan int, 1)
			maxC <- 0
			current := make(chan int, 1)
			current <- 0

			for _, n := range tc.allocs {
				wg.Add(1)
				go func() {
					defer wg.Done()
					release, err := p.reserve(ctx, n)
					if err != nil {
						t.Errorf("reserve(%d): %v", n, err)
						return
					}
					c := <-current
					c++
					current <- c
					m := <-maxC
					if c > m {
						m = c
					}
					maxC <- m
					time.Sleep(10 * time.Millisecond)
					c = <-current
					c--
					current <- c
					release()
				}()
			}
			wg.Wait()
			got := <-maxC
			if got > tc.want {
				t.Errorf("\n%s\nmaxConcurrent = %d, want at most %d", tc.reason, got, tc.want)
			}
		})
	}
}

func TestMemPoolContextCancelled(t *testing.T) {
	p := newMemPool(100)
	ctx := context.Background()

	// Fill the pool.
	release, err := p.reserve(ctx, 100)
	if err != nil {
		t.Fatalf("reserve(): %v", err)
	}

	// A second reserve with a cancelled context fails immediately.
	ctx2, cancel := context.WithCancel(context.Background())
	cancel()
	_, err = p.reserve(ctx2, 50)
	if err == nil {
		t.Fatal("reserve() with cancelled context should fail")
	}
	if want := "waiting for"; !containsStr(err.Error(), want) {
		t.Errorf("reserve() error = %q, want containing %q", err, want)
	}
	release()
}

// TestMemPoolNoLostWakeup guards against the pre-broadcast lost wakeup: a full
// pool releasing once must wake every waiter the freed bytes can satisfy, not
// just one. With total 100 and the pool full, two 40-unit reservations both fit
// after the 100-unit holder releases (40+40 <= 100); the old single-token wake
// woke only one and stranded the other until it timed out with memory free.
func TestMemPoolNoLostWakeup(t *testing.T) {
	const total = 100
	p := newMemPool(total)

	held, err := p.reserve(context.Background(), total)
	if err != nil {
		t.Fatalf("reserve(%d): %v", total, err)
	}

	// A generous but bounded deadline: on the fixed pool both waiters return in
	// microseconds; on the lost-wakeup bug the stranded waiter returns this
	// error at the deadline instead of blocking the test forever.
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()

	type result struct {
		release func()
		err     error
	}
	results := make(chan result, 2)
	for range 2 {
		go func() {
			release, err := p.reserve(ctx, 40)
			results <- result{release: release, err: err}
		}()
	}

	// Let both waiters block in the slow path before the single release, so a
	// lost wakeup would strand one of them.
	time.Sleep(100 * time.Millisecond)
	held()

	// Collect both results before releasing either: one release must admit both.
	// Do not release a winner first - on the buggy pool that second release
	// would itself wake the stranded waiter and mask the lost wakeup.
	got := make([]result, 0, 2)
	for range 2 {
		got = append(got, <-results)
	}
	for i, r := range got {
		if r.err != nil {
			t.Errorf("waiter %d: reserve(40) = %v, want it to fit after one release", i, r.err)
			continue
		}
		if r.release == nil {
			t.Errorf("waiter %d: reserve(40) returned a nil release func", i)
			continue
		}
		r.release()
	}
}

func TestFormatBytes(t *testing.T) {
	cases := map[string]struct {
		in   int64
		want string
	}{
		"Gi":   {in: 2 << 30, want: "2Gi"},
		"Mi":   {in: 512 << 20, want: "512Mi"},
		"Ki":   {in: 64 << 10, want: "64Ki"},
		"Bare": {in: 1000, want: "1000"},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			if got := formatBytes(tc.in); got != tc.want {
				t.Errorf("formatBytes(%d) = %q, want %q", tc.in, got, tc.want)
			}
		})
	}
}
