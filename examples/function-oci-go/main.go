// Package main is the function-oci-go guest: a Crossplane composition
// function in Go that function-oci, the OCI runtime, boots in a microVM once
// per request. fn.go is an ordinary function-sdk-go function; this file is
// the process contract around it (docs/oci/modules.md).
package main

import (
	"context"
	"fmt"
	"io"
	"os"

	"github.com/crossplane/function-sdk-go/logging"
	fnv1 "github.com/crossplane/function-sdk-go/proto/v1"
	"google.golang.org/protobuf/proto"
)

func main() {
	log, err := logging.NewLogger(false)
	if err != nil {
		_, _ = fmt.Fprintf(os.Stderr, "cannot create logger: %v\n", err)
		os.Exit(1)
	}
	os.Exit(run(log, os.Stdin, os.Stdout, os.Stderr))
}

// run is the process contract: the RunFunctionRequest bytes on standard
// input, the RunFunctionResponse bytes on standard output, log lines on
// standard error, and a zero exit status. The runtime turns any other
// status into a fatal result carrying the last line of standard error, so
// a failure to read, decode or encode is written there as one plain line.
// The logger writes JSON lines with level and msg, which the runtime logs
// at that level with the module and its digest attached.
func run(log logging.Logger, stdin io.Reader, stdout, stderr io.Writer) int {
	if err := serve(log, stdin, stdout); err != nil {
		// The line the runtime quotes; nothing is left to do about a
		// failure to write it.
		_, _ = fmt.Fprintln(stderr, err)
		return 1
	}
	return 0
}

// serve reads one request, runs the function and writes its response.
func serve(log logging.Logger, stdin io.Reader, stdout io.Writer) error {
	raw, err := io.ReadAll(stdin)
	if err != nil {
		return fmt.Errorf("cannot read RunFunctionRequest from standard input: %w", err)
	}
	req := &fnv1.RunFunctionRequest{}
	if err := proto.Unmarshal(raw, req); err != nil {
		return fmt.Errorf("cannot decode RunFunctionRequest from standard input: %w", err)
	}
	rsp, err := (&Function{log: log}).RunFunction(context.Background(), req)
	if err != nil {
		return fmt.Errorf("cannot run function: %w", err)
	}
	out, err := proto.Marshal(rsp)
	if err != nil {
		return fmt.Errorf("cannot encode RunFunctionResponse: %w", err)
	}
	if _, err := stdout.Write(out); err != nil {
		return fmt.Errorf("cannot write RunFunctionResponse to standard output: %w", err)
	}
	return nil
}
