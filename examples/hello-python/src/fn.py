# The hello-python guest's logic: the same greeting function as every
# example, over the protobuf messages protoc generated from the vendored
# crossplane proto (the pure-Python protobuf runtime - the C extension does
# not exist in wasm, and the fallback engages by itself). fetch_text
# resolves config.greetingUrl - wasi:http through the host on the wasm
# target, a test double natively - so this file tests under plain python.

from typing import Callable, List, Optional, Tuple

from google.protobuf import struct_pb2

from run_function_pb2 import (
    RunFunctionRequest,
    RunFunctionResponse,
    SEVERITY_FATAL,
    SEVERITY_NORMAL,
    STATUS_CONDITION_TRUE,
    TARGET_COMPOSITE,
    TARGET_COMPOSITE_AND_CLAIM,
)

DEFAULT_TTL_SECONDS = 60

FetchText = Callable[[str], str]
Log = Callable[[str, str, List[Tuple[str, str]]], None]


def run_function(req: RunFunctionRequest, fetch_text: FetchText, log: Log) -> RunFunctionResponse:
    """Adds a ConfigMap greeting the composite resource to the desired state.

    Raises a string on failure; handle() turns it into a fatal result.
    """
    tag = req.meta.tag
    log("info", "Running function", [("tag", tag)])

    config = struct_field(req.input, "config")
    greeting = string_field(config, "greeting", "cannot read config")
    if greeting is None:
        greeting = "hello"
    # greetingUrl fetches the greeting through the host instead - the
    # requires.egress grant of the module's manifest decides whether it may.
    url = string_field(config, "greetingUrl", "cannot read config")
    if url is not None:
        try:
            greeting = fetch_text(url)
        except Exception as e:
            raise RunError(f"cannot fetch greeting: {e}") from e

    if not req.observed.HasField("composite") or not req.observed.composite.HasField("resource"):
        raise RunError("cannot get observed composite resource: none in request")
    metadata = struct_field(req.observed.composite.resource, "metadata")
    name = string_field(metadata, "name", "cannot read metadata") or ""

    rsp = RunFunctionResponse()
    rsp.meta.tag = tag
    rsp.meta.ttl.seconds = DEFAULT_TTL_SECONDS
    rsp.desired.CopyFrom(req.desired)
    rsp.desired.resources["greeting"].resource.update(
        {
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "data": {"greeting": f"{greeting} {name}"},
        }
    )
    rsp.results.add(
        severity=SEVERITY_NORMAL,
        message=f"greeted {name}",
        target=TARGET_COMPOSITE,
    )
    rsp.conditions.add(
        type="FunctionSuccess",
        status=STATUS_CONDITION_TRUE,
        reason="Success",
        target=TARGET_COMPOSITE_AND_CLAIM,
    )
    return rsp


class RunError(Exception):
    """A failure whose message is the fatal result, worded like the other
    guests'."""


def struct_field(struct: Optional[struct_pb2.Struct], key: str) -> Optional[struct_pb2.Struct]:
    """Reads a Struct field's sub-object."""
    if struct is None or key not in struct.fields:
        return None
    v = struct.fields[key]
    if v.WhichOneof("kind") != "struct_value":
        return None
    return v.struct_value


def string_field(struct: Optional[struct_pb2.Struct], key: str, context: str) -> Optional[str]:
    """Reads a string field of a Struct, refusing non-strings the way the
    other guests word it."""
    if struct is None or key not in struct.fields:
        return None
    v = struct.fields[key]
    if v.WhichOneof("kind") != "string_value":
        raise RunError(f"{context}: {key} must be a string")
    return v.string_value


def handle(data: bytes, fetch_text: FetchText, log: Log) -> bytes:
    """Decode, run, encode. Every failure becomes a fatal result so the host
    can always decode the reply."""
    req = RunFunctionRequest()
    try:
        req.ParseFromString(data)
    except Exception as e:
        return fatal(None, f"cannot decode RunFunctionRequest: {e}").SerializeToString()
    try:
        return run_function(req, fetch_text, log).SerializeToString()
    except RunError as e:
        return fatal(req, str(e)).SerializeToString()


def fatal(req: Optional[RunFunctionRequest], message: str) -> RunFunctionResponse:
    rsp = RunFunctionResponse()
    if req is not None:
        rsp.meta.tag = req.meta.tag
        rsp.meta.ttl.seconds = DEFAULT_TTL_SECONDS
    rsp.results.add(severity=SEVERITY_FATAL, message=message, target=TARGET_COMPOSITE)
    return rsp
