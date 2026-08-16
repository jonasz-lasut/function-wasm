// Package main implements function-wasm, a Composition Function that runs a
// WebAssembly module supplied through its Input.
package main

import (
	"os"
	"path/filepath"
	"time"

	"github.com/alecthomas/kong"
	"github.com/spf13/afero"

	"github.com/crossplane/function-sdk-go"
	"github.com/crossplane/function-sdk-go/errors"
	"github.com/crossplane/function-sdk-go/response"

	"github.com/jonasz-lasut/function-wasm/internal/cache"
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

	ModuleDir          string        `help:"Directory served for 'path' module sources; unset refuses them." env:"MODULE_DIR"`
	MaxModuleSize      int           `help:"Maximum size of a module in MB." default:"128"`
	ModuleTimeout      time.Duration `help:"Maximum wall-clock time one module run may take." default:"30s"`
	DisableMemoryCache bool          `help:"Do not keep compiled modules in memory between requests; every request loads the module's compiled artifact from disk (milliseconds). For runtimes serving large Go modules, whose compiled form is well over 100 MB each." env:"DISABLE_MEMORY_CACHE"`
	ModuleMemoryLimit  int           `help:"Maximum linear memory of a running module in MB." default:"512"`
	CosignKey          string        `help:"PEM file with one or more cosign public keys. When set, only OCI modules carrying a cosign signature by one of the keys run; http and path sources are refused." env:"COSIGN_KEY" type:"existingfile"`
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

	eng, err := engine.New(engine.Config{
		Timeout:     c.ModuleTimeout,
		MemoryLimit: int64(c.ModuleMemoryLimit) << 20,
	})
	if err != nil {
		return err
	}
	defer eng.Close()

	blobs, compiled, err := openCaches()
	if err != nil {
		return err
	}

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
		Blobs:    blobs,
		Verifier: verifier,
	})
	if err != nil {
		return err
	}

	fn := &Function{
		log:      log,
		ttl:      ttl,
		engine:   eng,
		modules:  engine.NewCache(eng, engine.CacheOptions{Disk: compiled, NoMemory: c.DisableMemoryCache}),
		resolver: resolver,
	}
	return function.Serve(fn,
		function.Listen(c.Network, c.Address),
		function.MTLSCertificates(c.TLSCertsDir),
		function.Insecure(c.Insecure),
		function.MaxRecvMessageSize(c.MaxRecvMessageSize*1024*1024))
}

// openCaches prepares the two on-disk caches under cache.DefaultDir before
// the function serves: fetched modules by digest, and wasmtime artifacts
// under a directory named after the wasmtime version and host so an upgrade
// never loads foreign code; artifacts of other versions are removed. Both
// directories are created if missing — the function never runs without them.
func openCaches() (blobs, compiled *cache.Store, err error) {
	engineVersion := engine.Version()

	modulesDir := filepath.Join(cache.DefaultDir, cache.ModulesDir)
	compiledDir := filepath.Join(cache.DefaultDir, cache.CompiledDir)
	versionDir := filepath.Join(compiledDir, engineVersion)
	for _, d := range []struct{ path, what string }{
		{cache.DefaultDir, "cache"},
		{modulesDir, "modules cache"},
		{compiledDir, "compiled cache"},
		{versionDir, "compiled cache"},
	} {
		if _, err := os.Stat(d.path); os.IsNotExist(err) {
			err = os.Mkdir(d.path, 0750)
			if err != nil {
				return nil, nil, errors.Wrapf(err, "failed to create %s dir", d.what)
			}
		}
	}
	base := afero.NewBasePathFs(afero.NewOsFs(), cache.DefaultDir)
	if err := cache.RemoveOthers(base, cache.CompiledDir, engineVersion); err != nil {
		return nil, nil, errors.Wrapf(err, "cannot clean %s", compiledDir)
	}
	blobs, err = cache.Subdir(base, cache.ModulesDir, true)
	if err != nil {
		return nil, nil, err
	}
	compiled, err = cache.Subdir(base, filepath.Join(cache.CompiledDir, engineVersion), false)
	if err != nil {
		return nil, nil, err
	}
	return blobs, compiled, nil
}

func main() {
	ctx := kong.Parse(&CLI{}, kong.Description("A Crossplane composition function that runs a WebAssembly module supplied through its input in a wasmtime sandbox."))
	ctx.FatalIfErrorf(ctx.Run())
}
