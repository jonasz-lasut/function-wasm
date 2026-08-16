// Package main implements function-wasm, a Composition Function that runs a
// WebAssembly module supplied through its Input.
package main

import (
	"context"
	"net/http"
	"os"
	"path/filepath"
	"sync/atomic"
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
	"github.com/jonasz-lasut/function-wasm/internal/egress"
	"github.com/jonasz-lasut/function-wasm/internal/engine"
	"github.com/jonasz-lasut/function-wasm/internal/metrics"
	"github.com/jonasz-lasut/function-wasm/internal/module"
	"github.com/jonasz-lasut/function-wasm/internal/sandbox"
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

	// Readiness and the concurrent-runs bound.
	MaxConcurrentRuns int      `help:"Most module runs executing at once; a further request waits for a slot until its deadline, then fails with a fatal result. 0 leaves concurrency to the caller." default:"0" env:"MAX_CONCURRENT_RUNS"`
	HealthAddress     string   `help:"Address of the plain-HTTP health endpoints /livez and /readyz (ready once the caches are open and --warm-modules are loaded); empty disables them. The gRPC health service on the function port answers too, but speaks mTLS." default:":8081" env:"HEALTH_ADDRESS"`
	WarmModules       []string `help:"Modules loaded — resolved, verified, compiled or mapped from the artifact cache — before the health service reports Serving: OCI references pinned to their manifest digest (repo[:tag]@sha256:...) and, with --module-dir, path:<file> entries. Repeatable or comma-separated. One that fails to load is logged and loaded on its first request instead." env:"WARM_MODULES" sep:"," placeholder:"REF"`
	// Sandbox ceilings (docs/one-pager-sandbox.md): every capability is
	// switched on with --enable-sandbox-<feature>; a Composition asks for
	// less through the Input's sandbox, never more. Host directories are
	// deliberately not mountable into modules — no flag offers it.
	EnableSandboxPrivateTmp bool   `help:"Let Compositions give their modules a private, empty, writable /tmp for the duration of a request (sandbox.filesystem.privateTmp), created under $TMPDIR and removed afterwards." env:"ENABLE_SANDBOX_PRIVATE_TMP"`
	EnableSandboxEnv        bool   `help:"Let Compositions set the environment variables their modules see (sandbox.env); non-secret values only." env:"ENABLE_SANDBOX_ENV"`
	EnableSandboxEgress     bool   `help:"Let Compositions grant their modules HTTP(S) requests through the host (sandbox.egress.http): the host performs wasmfn.http requests within each Composition's grant and the egress policy. Off, any sandbox.egress grant is a fatal result." env:"ENABLE_SANDBOX_EGRESS"`
	SandboxEgressPolicy     string `help:"YAML or JSON file with the egress ceiling: hosts and hostPatterns a Composition may grant (any, when both are empty), blockedCIDRs and allowedCIDRs on top of the default block list (loopback, link-local, private, cluster and reserved ranges), and the per-run budgets timeout, maxRequests, maxResponseBytes, maxRedirects. Without it the defaults apply." env:"SANDBOX_EGRESS_POLICY" type:"existingfile"`
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
		Timeout:           c.ModuleTimeout,
		MemoryLimit:       int64(c.ModuleMemoryLimit) << 20,
		MaxConcurrentRuns: c.MaxConcurrentRuns,
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
	// The sandbox ceiling is checked once here — an unwritable $TMPDIR stops
	// the runtime instead of failing every request that asks for a /tmp.
	ceiling, err := sandbox.NewCeiling(sandbox.Options{
		EnablePrivateTmp: c.EnableSandboxPrivateTmp,
		EnableEnv:        c.EnableSandboxEnv,
	})
	if err != nil {
		return err
	}
	// The egress ceiling: only with the flag, so a runtime that never opted
	// in has no code path that dials out for a module.
	var egressCeiling *egress.Egress
	if c.EnableSandboxEgress {
		policy := egress.Policy{}
		if c.SandboxEgressPolicy != "" {
			if policy, err = egress.LoadPolicy(c.SandboxEgressPolicy); err != nil {
				return err
			}
		}
		if egressCeiling, err = egress.New(policy); err != nil {
			return err
		}
	} else if c.SandboxEgressPolicy != "" {
		return errors.New("--sandbox-egress-policy needs --enable-sandbox-egress")
	}
	if c.EnableSandboxPrivateTmp || c.EnableSandboxEnv || c.EnableSandboxEgress {
		egressCeilingText := "disabled"
		if egressCeiling != nil {
			egressCeilingText = egressCeiling.Describe()
		}
		log.Info("Sandbox grants enabled", "private-tmp", c.EnableSandboxPrivateTmp, "env", c.EnableSandboxEnv, "egress", c.EnableSandboxEgress, "egress-policy", c.SandboxEgressPolicy, "egress-ceiling", egressCeilingText)
	}

	fn := &Function{
		log:    log,
		ttl:    ttl,
		engine: eng,
		egress: egressCeiling,
		modules: engine.NewCache(eng, engine.CacheOptions{
			Disk:                  compiled,
			NoMemory:              !c.EnableMemoryCache,
			MaxEntries:            c.MaxCachedModules,
			MaxConcurrentCompiles: c.MaxConcurrentCompiles,
		}),
		resolver: resolver,
		sandbox:  ceiling,
	}
	// Readiness: the caches are open, the engine is up and the modules named
	// by --warm-modules are loaded. It is answered twice — by the gRPC health
	// service on the function port, and by /readyz on --health-address in
	// plain HTTP, because the function port speaks mTLS in every real
	// install and kubelet's gRPC probe dials without credentials. Warm-up
	// runs while the server already listens: the probes read "not ready" in
	// the meantime instead of a closed port (a liveness probe on the same
	// port would not survive minutes of compiling), a request that arrives
	// early is served cold or joins the load in progress, and a warm entry
	// that fails only costs its first request the load — never readiness.
	healthSrv := health.NewServer()
	var ready atomic.Bool
	setHealth := func(status healthgrpc.HealthCheckResponse_ServingStatus) {
		healthSrv.SetServingStatus("", status)
		healthSrv.SetServingStatus(fnv1.FunctionRunnerService_ServiceDesc.ServiceName, status)
		ready.Store(status == healthgrpc.HealthCheckResponse_SERVING)
	}
	setHealth(healthgrpc.HealthCheckResponse_NOT_SERVING)
	if c.HealthAddress != "" {
		go serveHealth(log, c.HealthAddress, &ready)
	}
	go func() {
		fn.warm(context.Background(), c.WarmModules, c.MaxConcurrentCompiles)
		setHealth(healthgrpc.HealthCheckResponse_SERVING)
	}()
	return function.Serve(fn,
		function.Listen(c.Network, c.Address),
		function.MTLSCertificates(c.TLSCertsDir),
		function.Insecure(c.Insecure),
		function.MaxRecvMessageSize(c.MaxRecvMessageSize*1024*1024),
		function.WithHealthServer(healthSrv))
}

// serveHealth answers /livez (the process is up) and /readyz (200 once
// ready, 503 while warming) in plain HTTP on address, for probes that cannot
// speak the function port's mTLS. It runs for the life of the process.
func serveHealth(log logging.Logger, address string, ready *atomic.Bool) {
	mux := http.NewServeMux()
	mux.HandleFunc("/livez", func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte("ok\n"))
	})
	mux.HandleFunc("/readyz", func(w http.ResponseWriter, _ *http.Request) {
		if !ready.Load() {
			w.WriteHeader(http.StatusServiceUnavailable)
			_, _ = w.Write([]byte("warming\n"))
			return
		}
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte("ok\n"))
	})
	srv := &http.Server{Addr: address, Handler: mux, ReadHeaderTimeout: 10 * time.Second}
	if err := srv.ListenAndServe(); err != nil {
		log.Info("Cannot serve health probes", "address", address, "error", err)
	}
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
