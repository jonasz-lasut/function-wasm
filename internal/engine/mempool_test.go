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
