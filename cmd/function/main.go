// Package main implements function-wasm, a Composition Function that runs a
// WebAssembly module supplied through its Input.
package main

import (
	"context"
	"io"
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

	"github.com/jonasz-lasut/function-wasm/internal/admission"
	"github.com/jonasz-lasut/function-wasm/internal/authz"
	"github.com/jonasz-lasut/function-wasm/internal/cache"
	"github.com/jonasz-lasut/function-wasm/internal/egress"
	"github.com/jonasz-lasut/function-wasm/internal/engine"
	"github.com/jonasz-lasut/function-wasm/internal/metrics"
	"github.com/jonasz-lasut/function-wasm/internal/module"
	"github.com/jonasz-lasut/function-wasm/internal/sandbox"
)

// sweepInterval is how often the on-disk caches are measured and bounded.
const sweepInterval = 10 * time.Minute

// CLI of this Function: serve (the default, so every existing invocation and
// a DeploymentRuntimeConfig's args keep working with no subcommand) and
// validate, which runs the runtime's own admission over Compositions offline
// against the same ceiling flags.
type CLI struct {
	Debug bool `short:"d" help:"Emit debug logs in addition to info logs."`

	Serve    ServeCmd    `cmd:"" default:"withargs" help:"Serve the function over gRPC (the default)."`
	Validate ValidateCmd `cmd:"" help:"Validate the function-wasm Inputs of Compositions against these flags, offline: the checks a request passes before its module is resolved, in the runtime's own words."`
}

// CeilingFlags are the operator's ceilings — what a Composition's Input is
// admitted against — shared by serve and validate, so the flags an operator
// passes to the runtime are the flags validate takes.
type CeilingFlags struct {
	ModuleDir         string        `help:"Directory served for 'path' module sources; unset refuses them." env:"MODULE_DIR"`
	MaxModuleSize     int           `help:"Maximum size of a module in MB." default:"128"`
	ModuleTimeout     time.Duration `help:"Maximum wall-clock time one module run may take." default:"30s"`
	ModuleMemoryLimit int           `help:"Maximum linear memory of a running module in MB." default:"512"`
	CosignKey         string        `help:"PEM file with one or more cosign public keys. When set, only OCI modules carrying a cosign signature by one of the keys run; http and path sources are refused." env:"COSIGN_KEY" type:"existingfile"`
	SandboxPolicyFile string        `help:"Cedar document with the operator's grant policy: which callers (by namespace, xrKind) a Composition may be granted a private /tmp, environment or egress for, and the SSRF CIDR block/allow rules (forbid/permit on Action::\"dialAddress\" with context.ip.isInRange(ip(...))/isLoopback()), which compile at load into the egress block list - no Cedar runs on the dial path. Evaluated default-deny after the --enable-sandbox-* floor, so it only tightens - a mounted ConfigMap satisfies it, and it is immutable for the process (restart to reload). Unset, no operator constraint applies." env:"SANDBOX_POLICY_FILE" type:"existingfile"`

	// Sandbox ceilings (docs/one-pager-sandbox.md): every capability is
	// switched on with --enable-sandbox-<feature>; a Composition asks for
	// less through the Input's sandbox, never more. Host directories are
	// deliberately not mountable into modules — no flag offers it.
	EnableSandboxPrivateTmp bool   `help:"Let Compositions give their modules a private, empty, writable /tmp for the duration of a request (sandbox.filesystem.privateTmp), created under $TMPDIR and removed afterwards." env:"ENABLE_SANDBOX_PRIVATE_TMP"`
	EnableSandboxEnv        bool   `help:"Let Compositions set the environment variables their modules see (sandbox.env); non-secret values only." env:"ENABLE_SANDBOX_ENV"`
	EnableSandboxEgress     bool   `help:"Let Compositions grant their modules HTTP(S) requests through the host (sandbox.egress.http): the host performs wasmfn.http requests within each Composition's grant and the egress policy. Off, any sandbox.egress grant is a fatal result." env:"ENABLE_SANDBOX_EGRESS"`
	SandboxEgressPolicy     string `help:"YAML or JSON file with the egress ceiling: hosts and hostPatterns a Composition may grant (any, when both are empty), blockedCIDRs and allowedCIDRs on top of the default block list (loopback, link-local, private, cluster and reserved ranges), and the per-run budgets timeout, maxRequests, maxResponseBytes, maxRedirects. Without it the defaults apply." env:"SANDBOX_EGRESS_POLICY" type:"existingfile"`
}

// engineConfig is the run budget the flags name; MaxConcurrentRuns is
// serve's alone.
func (c *CeilingFlags) engineConfig() engine.Config {
	return engine.Config{
		Timeout:     c.ModuleTimeout,
		MemoryLimit: int64(c.ModuleMemoryLimit) << 20,
	}
}

// ceilings builds the admission ceilings from the flags, checking them once:
// an unwritable $TMPDIR or an unreadable policy file stops the command
// instead of failing every request (or every step).
func (c *CeilingFlags) ceilings(log logging.Logger) (admission.Ceilings, error) {
	// The sandbox ceiling is checked once here — an unwritable $TMPDIR stops
	// the runtime instead of failing every request that asks for a /tmp.
	sandboxCeiling, err := sandbox.NewCeiling(sandbox.Options{
		EnablePrivateTmp: c.EnableSandboxPrivateTmp,
		EnableEnv:        c.EnableSandboxEnv,
	})
	if err != nil {
		return admission.Ceilings{}, err
	}
	// The operator's grant policy, compiled once. It narrows the grant floor;
	// absent, it adds no constraint. Its Action::"dialAddress" rules are the
	// SSRF CIDR block/allow list, compiled to prefixes here so a malformed rule
	// stops the command at startup (and function validate) rather than a dial,
	// and injected into the egress ceiling below - Cedar never runs on the dial
	// path.
	var operatorPolicy *authz.OperatorPolicy
	if c.SandboxPolicyFile != "" {
		if operatorPolicy, err = authz.LoadOperatorPolicy(c.SandboxPolicyFile); err != nil {
			return admission.Ceilings{}, err
		}
		log.Info("Operator grant policy loaded", "policy-file", c.SandboxPolicyFile)
		// With an operator policy the signature requirement is per-repository
		// (requireSignature), not --cosign-key's all-or-nothing: a repository
		// no rule names is not required to be signed. Say so plainly, and warn
		// loudly in the dangerous case - a policy with no requireSignature rule
		// leaves --cosign-key requiring nothing - so an operator who added a
		// policy for other reasons does not silently lose signature enforcement.
		switch {
		case c.CosignKey != "" && operatorPolicy.HasSignatureRules():
			log.Info("Signature requirement is governed per-repository by the operator policy (requireSignature); --cosign-key provides the keys but no longer requires every module", "policy-file", c.SandboxPolicyFile)
		case c.CosignKey != "":
			log.Info("WARNING: --cosign-key is set but the operator policy has no requireSignature rule, so no module is required to be signed; add a requireSignature permit, or remove --sandbox-policy-file to keep --cosign-key's all-or-nothing", "policy-file", c.SandboxPolicyFile)
		}
	}
	ipRules, err := operatorPolicy.CompileIPRules()
	if err != nil {
		return admission.Ceilings{}, err
	}
	// The egress ceiling: only with the flag, so a runtime that never opted
	// in has no code path that dials out for a module. The operator's Cedar
	// CIDR rules extend its block and allow lists alongside DefaultBlockedCIDRs.
	var egressCeiling *egress.Egress
	if c.EnableSandboxEgress {
		policy := egress.Policy{}
		if c.SandboxEgressPolicy != "" {
			if policy, err = egress.LoadPolicy(c.SandboxEgressPolicy); err != nil {
				return admission.Ceilings{}, err
			}
		}
		if egressCeiling, err = egress.New(policy, egress.WithBlockedCIDRs(ipRules.Blocked), egress.WithAllowedCIDRs(ipRules.Allowed)); err != nil {
			return admission.Ceilings{}, err
		}
	} else if c.SandboxEgressPolicy != "" {
		return admission.Ceilings{}, errors.New("--sandbox-egress-policy needs --enable-sandbox-egress")
	}
	if c.EnableSandboxPrivateTmp || c.EnableSandboxEnv || c.EnableSandboxEgress {
		egressCeilingText := "disabled"
		if egressCeiling != nil {
			egressCeilingText = egressCeiling.Describe()
		}
		log.Info("Sandbox grants enabled", "private-tmp", c.EnableSandboxPrivateTmp, "env", c.EnableSandboxEnv, "egress", c.EnableSandboxEgress, "egress-policy", c.SandboxEgressPolicy, "egress-ceiling", egressCeilingText)
	}
	return admission.Ceilings{Engine: c.engineConfig().WithDefaults(), Sandbox: sandboxCeiling, Egress: egressCeiling, Policy: operatorPolicy}, nil
}

// resolver builds the module resolver the flags describe: --module-dir,
// --max-module-size and --cosign-key, over the blob store (nil for a one-off
// validate). When an operator policy is present it also carries its
// per-repository signature requirement (--sandbox-policy-file's requireSignature),
// which replaces --cosign-key's all-or-nothing: the key still verifies, the
// policy decides which repositories must be signed.
func (c *CeilingFlags) resolver(blobs *cache.Store, policy *authz.OperatorPolicy) (*module.Resolver, error) {
	var verifier *module.Verifier
	if c.CosignKey != "" {
		v, err := module.LoadVerifier(c.CosignKey)
		if err != nil {
			return nil, err
		}
		verifier = v
	}
	opts := module.Options{
		Dir:      c.ModuleDir,
		MaxSize:  int64(c.MaxModuleSize) << 20,
		Blobs:    blobs,
		Verifier: verifier,
	}
	if policy != nil {
		opts.RequireSignature = policy.RequiresSignature
	}
	return module.NewResolver(opts)
}

// ServeCmd runs the function.
type ServeCmd struct {
	CeilingFlags `embed:""`

	Network            string         `help:"Network on which to listen for gRPC connections." default:"tcp"`
	Address            string         `help:"Address at which to listen for gRPC connections." default:":9443"`
	TLSCertsDir        string         `help:"Directory containing server certs (tls.key, tls.crt) and the CA used to verify client certificates (ca.crt)" env:"TLS_SERVER_CERTS_DIR"`
	Insecure           bool           `help:"Run without mTLS credentials. If you supply this flag --tls-server-certs-dir will be ignored."`
	MaxRecvMessageSize int            `help:"Maximum size of received messages in MB." default:"4"`
	TTL                *time.Duration `help:"Time to live for function responses the runtime itself produces (fatal results); a module sets the TTL of its own responses."`

	EnableMemoryCache     bool `help:"Keep compiled modules in memory between requests. Off (--enable-memory-cache=false), every request maps the module's compiled artifact from disk (milliseconds) and releases it afterwards." default:"true" negatable:"" env:"ENABLE_MEMORY_CACHE"`
	MaxCachedModules      int  `help:"Most compiled modules kept in memory at once; the least recently used is dropped beyond it. 0 leaves it to the idle timeout." default:"0" env:"MAX_CACHED_MODULES"`
	MaxConcurrentCompiles int  `help:"Most modules compiled at once. A compile already uses every core and about a gigabyte for a large Go module; further first requests wait their turn." default:"1" env:"MAX_CONCURRENT_COMPILES"`
	MaxCacheSize          int  `help:"Bound in MB on the on-disk caches together (fetched modules and compiled artifacts); the least recently used entries are removed past it, at startup and every ten minutes. 0 leaves them unbounded." default:"0" env:"MAX_CACHE_SIZE"`

	// Readiness and the concurrent-runs bound.
	MaxConcurrentRuns int      `help:"Most module runs executing at once; a further request waits for a slot until its deadline, then fails with a fatal result. 0 leaves concurrency to the caller." default:"0" env:"MAX_CONCURRENT_RUNS"`
	MaxTotalRunMemory int      `help:"Total linear-memory budget in MB across all running modules; a run reserves its effective limit (limits.memory or --module-memory-limit) before it starts and waits under its deadline when the pool is full. 0 means no bound." default:"0" env:"MAX_TOTAL_RUN_MEMORY"`
	HealthAddress     string   `help:"Address of the plain-HTTP health endpoints /livez and /readyz (ready once the caches are open and --warm-modules are loaded); empty disables them. The gRPC health service on the function port answers too, but speaks mTLS." default:":8081" env:"HEALTH_ADDRESS"`
	WarmModules       []string `help:"Modules loaded — resolved, verified, compiled or mapped from the artifact cache — before the health service reports Serving: OCI references pinned to their manifest digest (repo[:tag]@sha256:...) and, with --module-dir, path:<file> entries. Repeatable or comma-separated. One that fails to load is logged and loaded on its first request instead." env:"WARM_MODULES" sep:"," placeholder:"REF"`
}

// Run serves the function.
func (c *ServeCmd) Run(cli *CLI) error {
	log, err := function.NewLogger(cli.Debug)
	if err != nil {
		return err
	}
	ttl := response.DefaultTTL
	if c.TTL != nil {
		ttl = *c.TTL
	}

	cfg := c.engineConfig()
	cfg.MaxConcurrentRuns = c.MaxConcurrentRuns
	cfg.MaxTotalRunMemory = int64(c.MaxTotalRunMemory) << 20
	eng, err := engine.New(cfg)
	if err != nil {
		return err
	}
	defer eng.Close()

	blobs, compiled, manifests, err := openCaches()
	if err != nil {
		return err
	}

	ceilings, err := c.ceilings(log)
	if err != nil {
		return err
	}
	resolver, err := c.resolver(blobs, ceilings.Policy)
	if err != nil {
		return err
	}

	// The periodic sweep bounds the on-disk caches and, unconditionally on the
	// same interval, trims the in-memory per-digest maps (step slots, egress
	// rate limiters) so they do not grow one entry per module for the pod's
	// life. Built before the goroutine starts so it can pass them in.
	stepSlots := engine.NewStepSlots()
	go sweepCaches(log, []*cache.Store{blobs, compiled, manifests}, int64(c.MaxCacheSize)<<20, stepSlots, ceilings.Egress)

	fn := &Function{
		log:    log,
		ttl:    ttl,
		engine: eng,
		egress: ceilings.Egress,
		modules: engine.NewCache(eng, engine.CacheOptions{
			Disk:                  compiled,
			NoMemory:              !c.EnableMemoryCache,
			MaxEntries:            c.MaxCachedModules,
			MaxConcurrentCompiles: c.MaxConcurrentCompiles,
		}),
		resolver:  resolver,
		sandbox:   ceilings.Sandbox,
		policy:    ceilings.Policy,
		manifests: manifests,
		stepSlots: stepSlots,
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
// so removing the least recently used ones is always safe. On the same tick
// it trims the in-memory per-digest maps: the step-slot entries and, when
// egress is enabled (eg non-nil), the egress rate limiters. Those run
// unconditionally, independent of maxBytes, so both maps stay bounded even
// with the on-disk cap off (--max-cache-size 0). It runs for the life of the
// process.
func sweepCaches(log logging.Logger, stores []*cache.Store, maxBytes int64, stepSlots *engine.StepSlots, eg *egress.Egress) {
	for {
		freed, err := cache.Sweep(stores, maxBytes)
		if err != nil {
			log.Info("Cannot sweep the module caches", "error", err)
		} else if freed > 0 {
			log.Info("Swept the module caches", "freed", freed, "max", maxBytes)
		}
		metrics.CacheBytes.WithLabelValues(metrics.CacheBlob).Set(float64(stores[0].Bytes()))
		metrics.CacheBytes.WithLabelValues(metrics.CacheCompiledDisk).Set(float64(stores[1].Bytes()))
		stepSlots.SweepIdle()
		if eg != nil {
			eg.SweepRateLimiters()
		}
		time.Sleep(sweepInterval)
	}
}

// openCaches prepares the three on-disk caches under cache.DefaultDir before
// the function serves: fetched modules by digest; wasmtime artifacts under a
// directory named after the wasmtime version and host so an upgrade never
// loads foreign code — artifacts of other versions are removed once nothing
// has written them for a day (a rolling upgrade shares the volume between
// versions); and the module manifests, kilobytes per digest, kept beside the
// artifacts so a warm volume needs no registry read to learn what a module
// requires. The directories are created if missing — the function never
// runs without them.
func openCaches() (blobs, compiled, manifests *cache.Store, err error) {
	engineVersion := engine.Version()

	modulesDir := filepath.Join(cache.DefaultDir, cache.ModulesDir)
	compiledDir := filepath.Join(cache.DefaultDir, cache.CompiledDir)
	versionDir := filepath.Join(compiledDir, engineVersion)
	manifestsDir := filepath.Join(cache.DefaultDir, cache.ManifestsDir)
	for _, d := range []struct{ path, what string }{
		{cache.DefaultDir, "cache"},
		{modulesDir, "modules cache"},
		{compiledDir, "compiled cache"},
		{versionDir, "compiled cache"},
		{manifestsDir, "manifests cache"},
	} {
		if err := os.MkdirAll(d.path, 0750); err != nil {
			return nil, nil, nil, errors.Wrapf(err, "failed to create %s dir", d.what)
		}
	}
	base := afero.NewBasePathFs(afero.NewOsFs(), cache.DefaultDir)
	if err := cache.RemoveOthers(base, cache.CompiledDir, engineVersion, time.Now()); err != nil {
		return nil, nil, nil, errors.Wrapf(err, "cannot clean %s", compiledDir)
	}
	blobs, err = cache.OpenDir(modulesDir, true)
	if err != nil {
		return nil, nil, nil, err
	}
	compiled, err = cache.OpenDir(versionDir, false)
	if err != nil {
		return nil, nil, nil, err
	}
	manifests, err = cache.OpenDir(manifestsDir, false)
	if err != nil {
		return nil, nil, nil, err
	}
	return blobs, compiled, manifests, nil
}

// parser builds the kong parser main and the tests share.
func parser(cli *CLI, stdout io.Writer) *kong.Kong {
	return kong.Must(cli,
		kong.Name("function"),
		kong.Description("A Crossplane composition function that runs a WebAssembly module supplied through its input in a wasmtime sandbox."),
		kong.BindTo(stdout, (*io.Writer)(nil)),
	)
}

func main() {
	cli := &CLI{}
	p := parser(cli, os.Stdout)
	ctx, err := p.Parse(os.Args[1:])
	p.FatalIfErrorf(err)
	err = ctx.Run(cli)
	// validate reports its verdict through the exit code alone: 1 when a step
	// is refused, 2 when the tool itself failed; kong's FatalIfErrorf would
	// print the error and exit 1 for both.
	var exit exitError
	if errors.As(err, &exit) {
		os.Exit(exit.code)
	}
	p.FatalIfErrorf(err)
}
