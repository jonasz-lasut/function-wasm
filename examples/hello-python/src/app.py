# The world wiring, wasm-only: componentize-py generates the bindings for
# wit/function.wit (the wit_world module, the typed log import and the
# wasi:http types), and this module implements the world's `run` export.
# `run` is declared sync in this guest's wit (componentize-py takes the sync
# shape; a sync-lifted function satisfies the runtime's async world) - the
# fetch still runs on componentize-py's PollLoop over wasi's poll, and
# wasi:http@0.2's outgoing-handler rides the host's egress policy.

import asyncio

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

from fn import handle


def fetch_text(url: str) -> str:
    """GETs a URL through the host and returns the trimmed body - the
    counterpart of the other guests' get_text helpers."""
    loop = poll_loop.PollLoop()
    asyncio.set_event_loop(loop)
    return loop.run_until_complete(fetch(url))


async def fetch(url: str) -> str:
    if url.startswith("https://"):
        scheme, rest = Scheme_Https(), url.removeprefix("https://")
    elif url.startswith("http://"):
        scheme, rest = Scheme_Http(), url.removeprefix("http://")
    else:
        raise ValueError(f"GET {url}: only http and https URLs work")
    authority, _, path = rest.partition("/")

    req = OutgoingRequest(Fields.from_list([]))
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
    if status != 200:
        raise ValueError(f"GET {url}: status {status}")
    return body.decode(errors="replace").strip()


def host_log(level: str, msg: str, kv) -> None:
    log(LogLevel.DEBUG if level == "debug" else LogLevel.INFO, msg, kv)


class WitWorld(wit_world.WitWorld):
    def run(self, request: bytes) -> bytes:
        return handle(request, fetch_text, host_log)
