// Package main is the function-oci-go guest: a Crossplane composition
// function in Go that function-oci, the OCI runtime, boots in a microVM once
// per request and calls over gRPC (docs/oci/modules.md). fn.go is an ordinary
// function-sdk-go function; this file starts its server, as any function's
// main does.
package main

import (
	"flag"
	"fmt"
	"os"

	function "github.com/crossplane/function-sdk-go"
	"github.com/crossplane/function-sdk-go/logging"
)

func main() {
	debug := flag.Bool("debug", false, "Emit debug logs in addition to info logs.")
	address := flag.String("address", function.DefaultAddress, "Address at which to listen for gRPC connections.")
	certs := flag.String("tls-certs-dir", os.Getenv("TLS_SERVER_CERTS_DIR"), "Directory containing tls.crt, tls.key and ca.crt.")
	insecure := flag.Bool("insecure", false, "Run without mTLS credentials.")
	flag.Parse()

	log, err := logging.NewLogger(*debug)
	if err == nil {
		err = function.Serve(&Function{log: log},
			function.Listen(function.DefaultNetwork, *address),
			function.MTLSCertificates(*certs),
			function.Insecure(*insecure))
	}
	if err != nil {
		_, _ = fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
