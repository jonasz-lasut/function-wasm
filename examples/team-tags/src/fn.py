# The guest's logic: the owning team's record, read from a CMDB with that
# team's token, stamped as tags on every composed resource and summed up in
# the composite resource's status. The team is the composite resource's
# namespace, and its token is the key of that name in the step credential
# holding every team's token. It works over the protobuf messages protoc
# generated from the vendored crossplane proto (the pure-Python protobuf
# runtime - the C extension does not exist in wasm, and the fallback
# engages by itself). The CMDB request comes in as an argument - wasi:http
# through the host on the wasm target, a test double natively - so this
# file tests under plain python.

import json
from typing import Callable, Dict, List, NamedTuple, Optional, Tuple
from urllib.parse import quote

from google.protobuf import struct_pb2

from run_function_pb2 import (
    RunFunctionRequest,
    RunFunctionResponse,
    SEVERITY_FATAL,
    SEVERITY_NORMAL,
    SEVERITY_WARNING,
    TARGET_COMPOSITE,
)

DEFAULT_TTL_SECONDS = 60

# The pipeline step's credential holding the CMDB tokens: a Secret with one
# key per team namespace.
CREDENTIAL = "cmdb"


class Response(NamedTuple):
    status: int
    body: bytes


# GETs a URL with request headers. It raises for a request the host does
# not perform (no grant, a blocked address, a transport failure); any status
# the server answers with is a Response.
Fetch = Callable[[str, List[Tuple[str, str]]], Response]
Log = Callable[[str, str, List[Tuple[str, str]]], None]


class Team(NamedTuple):
    owner: str
    cost_center: str


class RunError(Exception):
    """A failure whose message becomes the fatal result."""


def run_function(req: RunFunctionRequest, fetch: Fetch, log: Log) -> RunFunctionResponse:
    """Tags every composed resource with its team's CMDB record.

    Raises RunError on failure; handle() turns it into a fatal result, so a
    team the CMDB cannot vouch for leaves the resources as they were.
    """
    tag = req.meta.tag
    if not req.observed.HasField("composite") or not req.observed.composite.HasField("resource"):
        raise RunError("cannot get observed composite resource: none in request")
    metadata = struct_field(req.observed.composite.resource, "metadata")
    name = string_field(metadata, "name", "cannot read metadata") or ""
    # The team is the namespace, never a field of the spec: the composite
    # resource's author writes its spec, so a team named there would let an
    # author in one team's namespace spend another team's token. Cluster
    # RBAC decides who may create a composite resource in which namespace.
    team = string_field(metadata, "namespace", "cannot read metadata")
    if not team:
        raise RunError(f"cannot read metadata: {name} has no namespace, which names its team")

    base = string_field(struct_field(req.input, "config"), "cmdbUrl", "cannot read config")
    if not base:
        raise RunError(f"cannot look up team {team}: config.cmdbUrl is not set")
    token = team_token(req, team)

    log("info", "Looking up team", [("team", team), ("composite", name)])
    record = lookup(team, base, token, fetch)
    tags = {"team": team, "owner": record.owner, "cost-center": record.cost_center}

    rsp = RunFunctionResponse()
    rsp.meta.tag = tag
    rsp.meta.ttl.seconds = DEFAULT_TTL_SECONDS
    rsp.desired.CopyFrom(req.desired)

    tagged = 0
    for resource_name in sorted(rsp.desired.resources):
        replaced = stamp(resource_name, rsp.desired.resources[resource_name].resource, tags)
        if replaced is None:
            continue
        tagged += 1
        # The CMDB is the record of who owns what: a value an earlier step
        # set for one of these keys - from the composite resource, say -
        # loses, and says so.
        for key, old in replaced:
            rsp.results.add(
                severity=SEVERITY_WARNING,
                message=f"{resource_name}: replaced tag {key}={old} with the CMDB's {key}={tags[key]}",
                target=TARGET_COMPOSITE,
            )

    status = rsp.desired.composite.resource.get_or_create_struct("status")
    status["cmdb"] = {"owner": record.owner, "costCenter": record.cost_center}

    rsp.results.add(
        severity=SEVERITY_NORMAL,
        message=(
            f"tagged {tagged} {'resource' if tagged == 1 else 'resources'} of team {team} "
            f"(owner {record.owner}, cost center {record.cost_center})"
        ),
        target=TARGET_COMPOSITE,
    )
    return rsp


def team_token(req: RunFunctionRequest, team: str) -> str:
    """Reads the team's token: the key named after the team's namespace in
    the step credential holding every team's token."""
    if CREDENTIAL not in req.credentials:
        raise RunError(
            f'cannot look up team {team}: the request carries no credential "{CREDENTIAL}"; '
            "declare it on the pipeline step"
        )
    credential = req.credentials[CREDENTIAL]
    if credential.WhichOneof("source") != "credential_data":
        raise RunError(f'cannot look up team {team}: credential "{CREDENTIAL}" has no data')
    # A Secret written from a file often ends in a newline, which a header
    # cannot carry.
    token = credential.credential_data.data.get(team, b"").strip()
    if not token:
        raise RunError(
            f'cannot look up team {team}: credential "{CREDENTIAL}" has no token '
            f"for namespace {team}"
        )
    try:
        return token.decode()
    except UnicodeDecodeError as e:
        raise RunError(
            f'cannot look up team {team}: credential "{CREDENTIAL}" has a token '
            f"for namespace {team} that is not UTF-8"
        ) from e


def lookup(team: str, base: str, token: str, fetch: Fetch) -> Team:
    """GETs <base>/teams/<team> with the token and reads the record."""
    # Quoted whole, so the request stays under /teams/ whatever the
    # namespace holds: the token goes wherever the URL points.
    url = f"{base.rstrip('/')}/teams/{quote(team, safe='')}"
    try:
        rsp = fetch(url, [("authorization", f"Bearer {token}")])
    except Exception as e:
        raise RunError(f"cannot look up team {team}: GET {url}: {e}") from e
    if rsp.status == 404:
        raise RunError(f"team {team} is not in the CMDB: GET {url}: status 404")
    if rsp.status != 200:
        raise RunError(f"cannot look up team {team}: GET {url}: status {rsp.status}")
    try:
        record = json.loads(rsp.body)
    except ValueError as e:
        raise RunError(f"cannot look up team {team}: the CMDB's answer is not JSON: {e}") from e
    if not isinstance(record, dict):
        raise RunError(f"cannot look up team {team}: the CMDB's answer is not an object")
    if record.get("team") != team:
        raise RunError(
            f"cannot look up team {team}: the CMDB's record is for team {json.dumps(record.get('team'))}"
        )
    fields: Dict[str, str] = {}
    for key in ("owner", "costCenter"):
        value = record.get(key)
        if not isinstance(value, str) or not value:
            raise RunError(f"cannot look up team {team}: the CMDB's record has no {key}")
        fields[key] = value
    return Team(owner=fields["owner"], cost_center=fields["costCenter"])


def stamp(
    resource_name: str, resource: struct_pb2.Struct, tags: Dict[str, str]
) -> Optional[List[Tuple[str, str]]]:
    """Sets tags on a composed resource's spec.forProvider.tags, keeping the
    tags already there. Returns the keys whose different value it replaced,
    or None for a resource that is not a managed resource (no
    spec.forProvider), which it leaves alone."""
    for_provider = struct_field(struct_field(resource, "spec"), "forProvider")
    if for_provider is None:
        return None
    existing = for_provider.fields.get("tags")
    if existing is not None and existing.WhichOneof("kind") != "struct_value":
        raise RunError(f"cannot tag {resource_name}: spec.forProvider.tags is not an object")
    current = for_provider.get_or_create_struct("tags")
    replaced = []
    for key, value in tags.items():
        old = current.fields.get(key)
        if old is not None and old.WhichOneof("kind") == "string_value" and old.string_value != value:
            replaced.append((key, old.string_value))
        current[key] = value
    return replaced


def struct_field(struct: Optional[struct_pb2.Struct], key: str) -> Optional[struct_pb2.Struct]:
    """Reads a Struct field's sub-object."""
    if struct is None or key not in struct.fields:
        return None
    v = struct.fields[key]
    if v.WhichOneof("kind") != "struct_value":
        return None
    return v.struct_value


def string_field(struct: Optional[struct_pb2.Struct], key: str, context: str) -> Optional[str]:
    """Reads a string field of a Struct; a non-string is an error naming
    it."""
    if struct is None or key not in struct.fields:
        return None
    v = struct.fields[key]
    if v.WhichOneof("kind") != "string_value":
        raise RunError(f"{context}: {key} must be a string")
    return v.string_value


def handle(data: bytes, fetch: Fetch, log: Log) -> bytes:
    """Decode, run, encode. Every failure becomes a fatal result so the host
    can always decode the reply."""
    req = RunFunctionRequest()
    try:
        req.ParseFromString(data)
    except Exception as e:
        return fatal(None, f"cannot decode RunFunctionRequest: {e}").SerializeToString()
    try:
        return run_function(req, fetch, log).SerializeToString()
    except RunError as e:
        return fatal(req, str(e)).SerializeToString()


def fatal(req: Optional[RunFunctionRequest], message: str) -> RunFunctionResponse:
    rsp = RunFunctionResponse()
    if req is not None:
        rsp.meta.tag = req.meta.tag
        rsp.meta.ttl.seconds = DEFAULT_TTL_SECONDS
    rsp.results.add(severity=SEVERITY_FATAL, message=message, target=TARGET_COMPOSITE)
    return rsp
