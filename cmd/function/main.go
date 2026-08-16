// Package main implements function-wasm, a Composition Function that runs a
// WebAssembly module supplied through its Input.
package main

import (
	"os"
	"path/filepath"
	"time"

	"github.com/alecthomas/kong"
	"github.com/spf13/afero"
	"google.golang.org/grpc/health"
	healthgrpc "google.golang.org/grpc/health/grpc_health_v1"

	"github.com/crossplane/function-sdk-go"
	"github.com/crossplane/function-sdk-go/errors"
	"github.com/crossplane/function-sdk-go/logging"
	fnv1 "github.com/crossplane/function-sdk-go/proto/v1"
	"github.com/crossplane/function-sdk-go/response"

	"github.com/jonasz-lasut/function-wasm/internal/cache"
	"github.com/jonasz-lasut/function-wasm/internal/engine"
	"github.com/jonasz-lasut/function-wasm/internal/metrics"
	"github.com/jonasz-lasut/function-wasm/internal/module"
)

// sweepInterval is how often the on-disk caches are measured and bounded.
const sweepInterval = 10 * time.Minute

// CLI of this Function.
type CLI struct {
	Debug bool `short:"d" help:"Emit debug logs in addition to info logs."`

	Network            string         `help:"Network on which to listen for gRPC connections." default:"tcp"`
	Address            string         `help:"Address at which to listen for gRPC connections." default:":9443"`
	TLSCertsDir        string         `help:"Directory containing server certs (tls.key, tls.crt) and the CA used to verify client certificates (ca.crt)" env:"TLS_SERVER_CERTS_DIR"`
	Insecure           bool           `help:"Run without mTLS credentials. If you supply this flag --tls-server-certs-dir will be ignored."`
	MaxRecvMessageSize int            `help:"Maximum size of received messages in MB." default:"4"`
	TTL                *time.Duration `help:"Time to live for function responses the runtime itself produces (fatal results); a module sets the TTL of its own responses."`

	ModuleDir             string        `help:"Directory served for 'path' module sources; unset refuses them." env:"MODULE_DIR"`
	MaxModuleSize         int           `help:"Maximum size of a module in MB." default:"128"`
	ModuleTimeout         time.Duration `help:"Maximum wall-clock time one module run may take." default:"30s"`
	EnableMemoryCache     bool          `help:"Keep compiled modules in memory between requests. Off (--enable-memory-cache=false), every request maps the module's compiled artifact from disk (milliseconds) and releases it afterwards." default:"true" negatable:"" env:"ENABLE_MEMORY_CACHE"`
	MaxCachedModules      int           `help:"Most compiled modules kept in memory at once; the least recently used is dropped beyond it. 0 leaves it to the idle timeout." default:"0" env:"MAX_CACHED_MODULES"`
	MaxConcurrentCompiles int           `help:"Most modules compiled at once. A compile already uses every core and about a gigabyte for a large Go module; further first requests wait their turn." default:"1" env:"MAX_CONCURRENT_COMPILES"`
	MaxCacheSize          int           `help:"Bound in MB on the on-disk caches together (fetched modules and compiled artifacts); the least recently used entries are removed past it, at startup and every ten minutes. 0 leaves them unbounded." default:"0" env:"MAX_CACHE_SIZE"`
	ModuleMemoryLimit     int           `help:"Maximum linear memory of a running module in MB." default:"512"`
	CosignKey             string        `help:"PEM file with one or more cosign public keys. When set, only OCI modules carrying a cosign signature by one of the keys run; http and path sources are refused." env:"COSIGN_KEY" type:"existingfile"`
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
	go sweepCaches(log, []*cache.Store{blobs, compiled}, int64(c.MaxCacheSize)<<20)

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
		log:    log,
		ttl:    ttl,
		engine: eng,
		modules: engine.NewCache(eng, engine.CacheOptions{
			Disk:                  compiled,
			NoMemory:              !c.EnableMemoryCache,
			MaxEntries:            c.MaxCachedModules,
			MaxConcurrentCompiles: c.MaxConcurrentCompiles,
		}),
		resolver: resolver,
	}
	// The gRPC health service answers Serving once the caches are open and
	// the engine is up, so a readiness probe (grpc_health_probe or a
	// Kubernetes gRPC probe on the function port) has something to ask.
	healthSrv := health.NewServer()
	healthSrv.SetServingStatus("", healthgrpc.HealthCheckResponse_SERVING)
	healthSrv.SetServingStatus(fnv1.FunctionRunnerService_ServiceDesc.ServiceName, healthgrpc.HealthCheckResponse_SERVING)
	return function.Serve(fn,
		function.Listen(c.Network, c.Address),
		function.MTLSCertificates(c.TLSCertsDir),
		function.Insecure(c.Insecure),
		function.MaxRecvMessageSize(c.MaxRecvMessageSize*1024*1024),
		function.WithHealthServer(healthSrv))
}

// sweepCaches bounds the on-disk caches to maxBytes, now and every ten
// minutes, and publishes their sizes. Entries are immutable and reproducible,
// so removing the least recently used ones is always safe. It runs for the
// life of the process.
func sweepCaches(log logging.Logger, stores []*cache.Store, maxBytes int64) {
	for {
		freed, err := cache.Sweep(stores, maxBytes)
		if err != nil {
			log.Info("Cannot sweep the module caches", "error", err)
		} else if freed > 0 {
			log.Info("Swept the module caches", "freed", freed, "max", maxBytes)
		}
		metrics.CacheBytes.WithLabelValues(metrics.CacheBlob).Set(float64(stores[0].Bytes()))
		metrics.CacheBytes.WithLabelValues(metrics.CacheCompiledDisk).Set(float64(stores[1].Bytes()))
		time.Sleep(sweepInterval)
	}
}

// openCaches prepares the two on-disk caches under cache.DefaultDir before
// the function serves: fetched modules by digest, and wasmtime artifacts
// under a directory named after the wasmtime version and host so an upgrade
// never loads foreign code; artifacts of other versions are removed once
// nothing has written them for a day (a rolling upgrade shares the volume
// between versions). Both directories are created if missing — the function
// never runs without them.
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
	if err := cache.RemoveOthers(base, cache.CompiledDir, engineVersion, time.Now()); err != nil {
		return nil, nil, errors.Wrapf(err, "cannot clean %s", compiledDir)
	}
	blobs, err = cache.OpenDir(modulesDir, true)
	if err != nil {
		return nil, nil, err
	}
	compiled, err = cache.OpenDir(versionDir, false)
	if err != nil {
		return nil, nil, err
	}
	return blobs, compiled, nil
}

func main() {
	ctx := kong.Parse(&CLI{}, kong.Description("A Crossplane composition function that runs a WebAssembly module supplied through its input in a wasmtime sandbox."))
	ctx.FatalIfErrorf(ctx.Run())
}
