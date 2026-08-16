// Package main implements function-wasm, a Composition Function that runs a
// WebAssembly module supplied through its Input.
package main

import (
	"path/filepath"
	"time"

	"github.com/alecthomas/kong"

	"github.com/crossplane/function-sdk-go"
	"github.com/crossplane/function-sdk-go/response"

	"github.com/jonasz-lasut/function-wasm/internal/engine"
	"github.com/jonasz-lasut/function-wasm/internal/module"
)

// CLI of this Function.
type CLI struct {
	Debug bool `short:"d" help:"Emit debug logs in addition to info logs."`

	Network            string         `help:"Network on which to listen for gRPC connections." default:"tcp"`
	Address            string         `help:"Address at which to listen for gRPC connections." default:":9443"`
	TLSCertsDir        string         `help:"Directory containing server certs (tls.key, tls.crt) and the CA used to verify client certificates (ca.crt)" env:"TLS_SERVER_CERTS_DIR"`
	Insecure           bool           `help:"Run without mTLS credentials. If you supply this flag --tls-server-certs-dir will be ignored."`
	MaxRecvMessageSize int            `help:"Maximum size of received messages in MB." default:"4"`
	TTL                *time.Duration `help:"Time to live for function responses the runtime itself produces (fatal results); a module sets the TTL of its own responses."`

	ModuleDir         string        `help:"Directory served for 'path' module sources; unset refuses them." env:"MODULE_DIR"`
	CacheDir          string        `help:"Directory for the on-disk caches of fetched modules and compiled code; back it with a volume to survive restarts. Unset keeps both in memory only." env:"CACHE_DIR"`
	ModuleCacheSize   int           `help:"Compiled modules kept in memory." default:"8"`
	ModuleTagTTL      time.Duration `help:"How long an OCI tag's resolution to a digest is reused." default:"5m"`
	MaxModuleSize     int           `help:"Maximum size of a module in MB." default:"128"`
	ModuleTimeout     time.Duration `help:"Maximum wall-clock time one module run may take." default:"30s"`
	ModuleMemoryLimit int           `help:"Maximum linear memory of a running module in MB." default:"512"`
	CosignKey         string        `help:"PEM file with one or more cosign public keys. When set, only OCI modules carrying a cosign signature by one of the keys run; http and path sources are refused." env:"COSIGN_KEY" type:"existingfile"`
}

// Run this Function.
func (c *CLI) Run() error {
	log, err := function.NewLogger(c.Debug)
	if err != nil {
		return err
	}
	ttl := response.DefaultTTL
	if c.TTL != nil {
		ttl = *c.TTL
	}

	var wasmtimeCache, blobCache string
	if c.CacheDir != "" {
		wasmtimeCache = filepath.Join(c.CacheDir, "wasmtime")
		blobCache = filepath.Join(c.CacheDir, "modules")
	}
	eng, err := engine.New(engine.Config{
		Timeout:     c.ModuleTimeout,
		MemoryLimit: int64(c.ModuleMemoryLimit) << 20,
		CacheDir:    wasmtimeCache,
	})
	if err != nil {
		return err
	}
	defer eng.Close()

	var verifier *module.Verifier
	if c.CosignKey != "" {
		verifier, err = module.LoadVerifier(c.CosignKey)
		if err != nil {
			return err
		}
	}
	resolver, err := module.NewResolver(module.Options{
		Dir:      c.ModuleDir,
		MaxSize:  int64(c.MaxModuleSize) << 20,
		TagTTL:   c.ModuleTagTTL,
		BlobDir:  blobCache,
		Verifier: verifier,
	})
	if err != nil {
		return err
	}

	fn := &Function{
		log:      log,
		ttl:      ttl,
		engine:   eng,
		modules:  engine.NewCache(c.ModuleCacheSize),
		resolver: resolver,
	}
	return function.Serve(fn,
		function.Listen(c.Network, c.Address),
		function.MTLSCertificates(c.TLSCertsDir),
		function.Insecure(c.Insecure),
		function.MaxRecvMessageSize(c.MaxRecvMessageSize*1024*1024))
}

func main() {
	ctx := kong.Parse(&CLI{}, kong.Description("A Crossplane composition function that runs a WebAssembly module supplied through its input in a wasmtime sandbox."))
	ctx.FatalIfErrorf(ctx.Run())
}
