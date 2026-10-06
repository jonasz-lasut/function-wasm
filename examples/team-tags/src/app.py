# The world wiring, wasm-only: componentize-py generates the bindings for
# wit/world.wit (the wit_world module, the typed log import and the
# wasi:http types), and this module implements the world's `run` export.
# `run` is declared sync in this guest's wit (componentize-py takes the sync
# shape; a sync-lifted function satisfies the runtime's async world) - the
# CMDB request still runs on componentize-py's PollLoop over wasi's poll,
# and wasi:http@0.2's outgoing-handler rides the host's egress policy.

import asyncio
from typing import List, Tuple

import poll_loop
import wit_world
from wit_world import LogLevel, log
from wit_world.imports.types import (
    Fields,
    Method_Get,
    OutgoingRequest,
    Scheme_Http,
    Scheme_Https,
)

from fn import Response, handle


def fetch(url: str, headers: List[Tuple[str, str]]) -> Response:
    """GETs a URL through the host and returns its status and body."""
    loop = poll_loop.PollLoop()
    asyncio.set_event_loop(loop)
    return loop.run_until_complete(get(url, headers))


async def get(url: str, headers: List[Tuple[str, str]]) -> Response:
    if url.startswith("https://"):
        scheme, rest = Scheme_Https(), url.removeprefix("https://")
    elif url.startswith("http://"):
        scheme, rest = Scheme_Http(), url.removeprefix("http://")
    else:
        raise ValueError(f"GET {url}: only http and https URLs work")
    authority, _, path = rest.partition("/")

    req = OutgoingRequest(Fields.from_list([(k, v.encode()) for k, v in headers]))
    req.set_method(Method_Get())
    req.set_scheme(scheme)
    req.set_authority(authority)
    req.set_path_with_query(f"/{path}")
    rsp = await poll_loop.send(req)
    status = rsp.status()
    stream = poll_loop.Stream(rsp.consume())
    body = b""
    while (chunk := await stream.next()) is not None:
        body += chunk
    return Response(status, body)


# The world's levels by the names fn.py logs with; any other name logs at
# info.
LEVELS = {
    "debug": LogLevel.DEBUG,
    "info": LogLevel.INFO,
    "warn": LogLevel.WARN,
    "error": LogLevel.ERROR,
}


def host_log(level: str, msg: str, kv) -> None:
    log(LEVELS.get(level, LogLevel.INFO), msg, kv)


class WitWorld(wit_world.WitWorld):
    def run(self, request: bytes) -> bytes:
        return handle(request, fetch, host_log)
