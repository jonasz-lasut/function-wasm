package egress

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/netip"
	"net/url"
	"strings"
	"sync/atomic"
	"time"

	"github.com/crossplane/function-sdk-go/logging"

	"github.com/jonasz-lasut/function-wasm/internal/egress/wire"
	"github.com/jonasz-lasut/function-wasm/internal/metrics"
)

// Request and Response are the wasmfn.http payloads (internal/egress/wire).
type (
	Request  = wire.Request
	Response = wire.Response
)

// Headers a guest may not set: the host owns the connection and the
// framing, and a Host header would let a request name one host and reach
// another.
var reservedHeaders = []string{"Host", "Content-Length", "Connection", "Transfer-Encoding", "Upgrade", "Keep-Alive", "Proxy-Connection", "Te", "Trailer"}

// Client performs one run's requests within its Grant and the ceiling's
// budgets, and writes the audit line for each. One per Run.
type Client struct {
	grant  *Grant
	digest string // module digest, for rate limiting
	log    logging.Logger

	requests atomic.Int64
	// overBudget remembers that this run's request budget was exhausted, so
	// the first refusal is an info line and a guest that keeps asking does
	// not flood the log.
	overBudget atomic.Bool
	http       *http.Client
}

// Client returns the per-run Client for this grant, logging through log
// (which carries the module reference and digest). digest identifies the
// module for process-wide rate limiting.
func (g *Grant) Client(log logging.Logger, digest string) *Client {
	c := &Client{grant: g, digest: digest, log: log}
	c.http = &http.Client{
		Transport: g.egress.transport(),
		// Every hop is checked like the first request: the redirect target
		// must be within the grant, and the redirected method (GET after a
		// 303, say) must be admitted for it. The dialer checks the target's
		// addresses again.
		CheckRedirect: func(req *http.Request, via []*http.Request) error {
			if len(via) > g.egress.budget.maxRedirects {
				return budgetError{fmt.Sprintf("stopped after %d redirects (maxRedirects)", g.egress.budget.maxRedirects)}
			}
			if err := g.admit(req.Method, req.URL); err != nil {
				return refusedError{msg: fmt.Sprintf("redirect to %s refused: %s", req.URL.Redacted(), err)}
			}
			// Every hop is a request the module made: it gets its own audit
			// line, since the host that finally answers is the one that saw
			// the request.
			if log != nil {
				log.Info("Module HTTP redirect", "method", req.Method, "host", req.URL.Hostname(), "path", pathOrRoot(req.URL.Path), "hop", len(via))
			}
			return nil
		},
	}
	return c
}

// Do performs req. It never returns a Go error: whatever stops the request
// is a Response with Status 0 and an Error, so a guest always gets a
// well-formed answer and never a trap. ctx bounds the whole request; the
// policy's timeout is applied on top.
func (c *Client) Do(ctx context.Context, req *Request) *Response {
	start := time.Now()
	rsp, outcome, u, detail := c.do(ctx, req)
	metrics.HTTPRequests.WithLabelValues(outcome).Inc()
	// The audit line: method, host and path (never the query, the headers or
	// the body), the status, the byte count and the outcome. What the guest
	// is told is in error; what only the operator should see — the address a
	// name resolved to and the block-list entry that refused it — in detail.
	kv := []any{"method", methodOf(req), "outcome", outcome, "duration", time.Since(start).String()}
	if u != nil {
		kv = append(kv, "host", u.Hostname(), "path", pathOrRoot(u.Path))
	}
	if rsp.Error != "" {
		kv = append(kv, "error", rsp.Error)
	} else {
		kv = append(kv, "status", rsp.Status, "bytes", len(rsp.Body))
	}
	if detail != "" {
		kv = append(kv, "detail", detail)
	}
	if c.log != nil {
		// A run that exhausted its request budget and keeps asking gets one
		// info line, then debug lines: the guest is looping, not the host.
		if outcome == metrics.OutcomeBudget && strings.Contains(rsp.Error, "maxRequests") && c.overBudget.Swap(true) {
			c.log.Debug("Module HTTP request", kv...)
		} else {
			c.log.Info("Module HTTP request", kv...)
		}
	}
	return rsp
}

func (c *Client) do(ctx context.Context, req *Request) (rsp *Response, outcome string, u *url.URL, detail string) {
	budget := c.grant.egress.budget
	if n := c.requests.Add(1); n > int64(budget.maxRequests) {
		return &Response{Error: fmt.Sprintf("sandbox.egress: this run already made %d requests (maxRequests)", budget.maxRequests)}, metrics.OutcomeBudget, nil, ""
	}
	if rl := c.grant.egress.rateLimits; rl != nil && !rl.allow(c.digest) {
		return &Response{Error: "sandbox.egress: the module's request rate exceeds the egress policy's rateLimit"}, metrics.OutcomeBudget, nil, ""
	}
	u, err := url.Parse(req.URL)
	if err != nil {
		return &Response{Error: fmt.Sprintf("sandbox.egress: cannot parse URL: %s", err)}, metrics.OutcomeError, nil, ""
	}
	method := methodOf(req)
	if err := c.grant.admit(method, u); err != nil {
		return &Response{Error: err.Error()}, metrics.OutcomeRefused, u, ""
	}

	// The policy's timeout applies on top of the run's remaining deadline;
	// the refusal names whichever was the shorter.
	limit := fmt.Sprintf("its %s timeout", budget.timeout)
	if d, ok := ctx.Deadline(); ok && time.Until(d) < budget.timeout {
		limit = "the run's remaining deadline"
	}
	ctx, cancel := context.WithTimeout(ctx, budget.timeout)
	defer cancel()
	hreq, err := http.NewRequestWithContext(ctx, method, u.String(), bytes.NewReader(req.Body))
	if err != nil {
		return &Response{Error: fmt.Sprintf("sandbox.egress: cannot build request: %s", err)}, metrics.OutcomeError, u, ""
	}
	hreq.Header = make(http.Header, len(req.Headers))
	for k, vs := range req.Headers {
		hreq.Header[http.CanonicalHeaderKey(k)] = vs
	}
	for _, h := range reservedHeaders {
		hreq.Header.Del(h)
	}

	hrsp, err := c.http.Do(hreq)
	if err != nil {
		rsp, outcome, detail := classify(err, limit)
		return rsp, outcome, u, detail
	}
	defer hrsp.Body.Close() //nolint:errcheck // Nothing to do about a close error on a fully read body.
	body, err := io.ReadAll(io.LimitReader(hrsp.Body, budget.maxResponseBytes+1))
	if err != nil {
		rsp, outcome, detail := classify(err, limit)
		return rsp, outcome, u, detail
	}
	if int64(len(body)) > budget.maxResponseBytes {
		return &Response{Error: fmt.Sprintf("sandbox.egress: the response body exceeds %d bytes (maxResponseBytes)", budget.maxResponseBytes)}, metrics.OutcomeBudget, u, ""
	}
	return &Response{Status: hrsp.StatusCode, Headers: hrsp.Header, Body: body}, metrics.OutcomeOK, u, ""
}

// classify renders a transport error for the guest and picks its metrics
// outcome in one pass over the error taxonomy, so a failed request is
// classified once: a refusal keeps its message — and its detail stays with
// the operator, returned separately for the audit line — a budget error
// keeps its message, a deadline names the limit that applied, and anything
// else is prefixed so a guest can tell the host's refusal from its own bug.
// A budget error and a deadline share the same OutcomeBudget label.
func classify(err error, limit string) (rsp *Response, outcome, detail string) {
	var refused refusedError
	var over budgetError
	switch {
	case errors.As(err, &refused):
		return &Response{Error: refused.msg}, metrics.OutcomeRefused, refused.detail
	case errors.As(err, &over):
		return &Response{Error: over.msg}, metrics.OutcomeBudget, ""
	case errors.Is(err, context.DeadlineExceeded):
		return &Response{Error: "sandbox.egress: the request exceeded " + limit}, metrics.OutcomeBudget, ""
	}
	// The url.Error wrapper only shapes the guest's message; the outcome is
	// error either way.
	var uerr *url.Error
	if errors.As(err, &uerr) {
		err = uerr.Err
	}
	return &Response{Error: "sandbox.egress: " + err.Error()}, metrics.OutcomeError, ""
}

func methodOf(req *Request) string {
	if req.Method == "" {
		return http.MethodGet
	}
	return strings.ToUpper(req.Method)
}

// refusedError and budgetError travel out of the dialer and the redirect
// check through net/http's wrapping so Do can classify them. A refusal's
// msg is for the guest; detail — the address a name resolved to, the
// block-list entry — is for the operator's audit line only: a module must
// not learn the cluster's address layout from what the policy refuses.
type refusedError struct{ msg, detail string }

func (e refusedError) Error() string { return e.msg }

type budgetError struct{ msg string }

func (e budgetError) Error() string { return e.msg }

// transport is the one http.Transport of the ceiling: its dialer resolves
// names itself, refuses every address the block list names and dials the
// address it checked — never a name a second time — so a name cannot rebind
// between the check and the connection. TLS is the transport's, against the
// host's roots and the URL's host name. No proxy: the host must see the
// destination address to judge it.
func (e *Egress) transport() http.RoundTripper {
	e.transportOnce.Do(func() {
		e.rt = &http.Transport{
			Proxy:                 nil,
			DialContext:           e.dial,
			ForceAttemptHTTP2:     true,
			MaxIdleConns:          32,
			IdleConnTimeout:       90 * time.Second,
			TLSHandshakeTimeout:   10 * time.Second,
			ExpectContinueTimeout: time.Second,
			// maxResponseBytes bounds the body; this bounds the headers,
			// which the guest also receives.
			MaxResponseHeaderBytes: maxResponseHeaderBytes,
		}
	})
	return e.rt
}

// maxResponseHeaderBytes bounds a response's header block: the body budget
// is the policy's, the headers are the host's to cap.
const maxResponseHeaderBytes = 64 << 10

func (e *Egress) dial(ctx context.Context, network, addr string) (net.Conn, error) {
	host, port, err := net.SplitHostPort(addr)
	if err != nil {
		return nil, refusedError{msg: "sandbox.egress: " + err.Error()}
	}
	var ips []netip.Addr
	if ip, err := netip.ParseAddr(host); err == nil {
		ips = []netip.Addr{ip}
	} else {
		ips, err = net.DefaultResolver.LookupNetIP(ctx, "ip", host)
		if err != nil {
			return nil, err
		}
	}
	if len(ips) == 0 {
		return nil, fmt.Errorf("no addresses for %s", host)
	}
	// Every address is judged, not just the first: a name that resolves to a
	// public and a private address is refused outright. Judging happens on
	// the bare address — an IPv6 zone (fe80::1%eth0, or [::1%25lo] written
	// into a URL) makes netip's prefix checks answer false, and a guest has no
	// business naming one of the host's interfaces — and the guest is told
	// only that the policy refused; which address and which entry stay in
	// the audit line.
	for i, ip := range ips {
		if ip.Zone() != "" {
			return nil, refusedError{msg: fmt.Sprintf("sandbox.egress: %s names an address with a zone, which is not dialled", host), detail: fmt.Sprintf("%s resolves to zoned address %s", host, ip)}
		}
		ips[i] = ip.Unmap()
		if ips[i].IsUnspecified() {
			return nil, refusedError{msg: fmt.Sprintf("sandbox.egress: %s resolves to an address the egress policy blocks", host), detail: fmt.Sprintf("%s resolves to the unspecified address %s", host, ip)}
		}
		if by := e.blockedBy(ips[i]); by != "" {
			return nil, refusedError{msg: fmt.Sprintf("sandbox.egress: %s resolves to an address the egress policy blocks", host), detail: fmt.Sprintf("%s resolves to %s, blocked by %s", host, ips[i], by)}
		}
	}
	dialer := &net.Dialer{Timeout: 10 * time.Second}
	var lastErr error
	for _, ip := range ips {
		conn, err := dialer.DialContext(ctx, network, net.JoinHostPort(ip.String(), port))
		if err == nil {
			return conn, nil
		}
		lastErr = err
	}
	return nil, lastErr
}
