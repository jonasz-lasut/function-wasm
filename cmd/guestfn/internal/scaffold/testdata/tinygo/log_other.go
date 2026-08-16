//go:build !wasip1

package main

import "os"

func stderrSink(_ int32, payload []byte) {
	_, _ = os.Stderr.Write(append(payload, '\n'))
}
