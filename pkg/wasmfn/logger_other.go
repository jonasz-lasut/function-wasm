//go:build !wasip1

package wasmfn

import (
	"fmt"
	"os"
)

func stderrSink(level int32, payload []byte) {
	name := "info"
	if level == levelDebug {
		name = "debug"
	}
	fmt.Fprintf(os.Stderr, "wasmfn %s %s\n", name, payload)
}
