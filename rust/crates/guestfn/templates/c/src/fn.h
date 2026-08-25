// The function this guest runs. Edit run_function (fn.c); the ABI glue
// (wasmfn.h) decodes the request, calls it and encodes the response.
#ifndef FN_H
#define FN_H

#include "run_function.pb.h"

// run_function composes a response to req into rsp, which the glue hands over
// zero-initialised except for meta (the request's tag and a 60 s TTL), and
// returns NULL - or the message of a fatal result, in which case the glue
// discards rsp. What it puts in rsp is heap or static memory that is never
// freed: a fresh instance serves each request.
const char *run_function(const fnv1_RunFunctionRequest *req, fnv1_RunFunctionResponse *rsp);

#endif
