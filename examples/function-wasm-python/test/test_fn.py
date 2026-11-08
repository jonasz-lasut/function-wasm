# Native tests for the guest's logic under plain python (unittest): a
# recording CMDB double stands in for the world's wasi:http import, and a
# no-op for the log import.

import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent.parent / "src"))
sys.path.insert(0, str(Path(__file__).parent.parent / "src" / "gen"))

from fn import Response, RunError, handle, run_function  # noqa: E402
from run_function_pb2 import (  # noqa: E402
    SEVERITY_FATAL,
    SEVERITY_NORMAL,
    SEVERITY_WARNING,
    TARGET_COMPOSITE,
    RunFunctionRequest,
    RunFunctionResponse,
)

CMDB = "http://cmdb.test"
# The shared Secret behind the step credential cmdb: one token per team
# namespace.
TOKENS = {"payments": b"payments-token", "search": b"search-token"}
PAYMENTS = {"team": "payments", "owner": "alice@example.com", "costCenter": "cc-4711"}
SEARCH = {"team": "search", "owner": "bob@example.com", "costCenter": "cc-2087"}


def log(level, msg, kv):
    pass


class FakeCMDB:
    """Answers GET <CMDB>/teams/<team> from a dict of records, a 404 for any
    other team, and records every request it is asked to make."""

    def __init__(self, records=None, status=None, body=None, error=None):
        self.records = {"payments": PAYMENTS, "search": SEARCH} if records is None else records
        self.status, self.body, self.error = status, body, error
        self.requests = []

    def __call__(self, url, headers):
        self.requests.append((url, headers))
        if self.error is not None:
            raise self.error
        if self.status is not None:
            return Response(self.status, self.body or b"")
        team = url.removeprefix(f"{CMDB}/teams/")
        if team not in self.records:
            return Response(404, b"404 page not found")
        return Response(200, json.dumps(self.records[team]).encode())


def request(namespace="payments", spec=None, config=None, resources=None, tokens=TOKENS):
    req = RunFunctionRequest()
    req.meta.tag = "t"
    metadata = {"name": "invoices"}
    if namespace is not None:
        metadata["namespace"] = namespace
    req.observed.composite.resource.update(
        {
            "apiVersion": "storage.example.org/v1alpha1",
            "kind": "Bucket",
            "metadata": metadata,
            "spec": {"region": "eu-central-1"} if spec is None else spec,
        }
    )
    req.input.update({"config": {"cmdbUrl": CMDB} if config is None else config})
    if tokens is not None:
        # Set even when empty: Crossplane sends a Secret without keys as
        # credential data with no entries.
        req.credentials["cmdb"].credential_data.SetInParent()
        req.credentials["cmdb"].credential_data.data.update(tokens)
    if resources is None:
        resources = {"bucket": bucket()}
    for name, resource in resources.items():
        req.desired.resources[name].resource.update(resource)
    return req


def bucket(tags=None):
    for_provider = {"region": "eu-central-1"}
    if tags is not None:
        for_provider["tags"] = tags
    return {
        "apiVersion": "s3.aws.m.upbound.io/v1beta1",
        "kind": "Bucket",
        "spec": {"forProvider": for_provider},
    }


def tags_of(rsp, name="bucket"):
    resource = rsp.desired.resources[name].resource
    return dict(resource["spec"]["forProvider"]["tags"].items())


def results_of(rsp):
    return [(r.severity, r.message) for r in rsp.results]


class TestRunFunction(unittest.TestCase):
    def test_sends_the_namespaces_token_to_its_teams_record(self):
        cmdb = FakeCMDB()
        run_function(request(), cmdb, log)
        self.assertEqual(
            cmdb.requests,
            [(f"{CMDB}/teams/payments", [("authorization", "Bearer payments-token")])],
        )

    def test_picks_the_token_by_namespace(self):
        cmdb = FakeCMDB()
        rsp = run_function(request(namespace="search"), cmdb, log)
        self.assertEqual(
            cmdb.requests,
            [(f"{CMDB}/teams/search", [("authorization", "Bearer search-token")])],
        )
        self.assertEqual(tags_of(rsp)["team"], "search")

    def test_a_team_named_in_the_spec_is_not_the_team(self):
        # The composite resource's author writes its spec: naming another
        # team there must not spend that team's token.
        cmdb = FakeCMDB()
        spec = {"region": "eu-central-1", "team": "payments"}
        rsp = run_function(request(namespace="search", spec=spec), cmdb, log)
        self.assertEqual(
            cmdb.requests,
            [(f"{CMDB}/teams/search", [("authorization", "Bearer search-token")])],
        )
        self.assertEqual(tags_of(rsp)["team"], "search")

    def test_a_token_written_with_a_trailing_newline(self):
        cmdb = FakeCMDB()
        run_function(request(tokens={"payments": b"payments-token\n"}), cmdb, log)
        self.assertEqual(cmdb.requests[0][1], [("authorization", "Bearer payments-token")])

    def test_base_url_with_a_trailing_slash(self):
        cmdb = FakeCMDB()
        run_function(request(config={"cmdbUrl": f"{CMDB}/"}), cmdb, log)
        self.assertEqual(cmdb.requests[0][0], f"{CMDB}/teams/payments")

    def test_team_cannot_leave_the_teams_path(self):
        # The token goes wherever the URL points: a team is one path
        # segment, never a way to another of the CMDB's endpoints.
        cmdb = FakeCMDB()
        with self.assertRaises(RunError):
            run_function(request(namespace="../admin", tokens={"../admin": b"t"}), cmdb, log)
        self.assertEqual(cmdb.requests[0][0], f"{CMDB}/teams/..%2Fadmin")

    def test_stamps_the_teams_tags(self):
        rsp = run_function(request(), FakeCMDB(), log)
        self.assertEqual(
            tags_of(rsp),
            {"team": "payments", "owner": "alice@example.com", "cost-center": "cc-4711"},
        )
        self.assertEqual(rsp.meta.tag, "t")
        self.assertEqual(rsp.meta.ttl.seconds, 60)
        self.assertEqual(
            results_of(rsp),
            [
                (
                    SEVERITY_NORMAL,
                    "tagged 1 resource of team payments (owner alice@example.com, cost center cc-4711)",
                )
            ],
        )
        self.assertEqual(rsp.results[0].target, TARGET_COMPOSITE)

    def test_keeps_the_tags_already_there_and_the_rest_of_the_resource(self):
        rsp = run_function(
            request(resources={"bucket": bucket(tags={"environment": "production"})}),
            FakeCMDB(),
            log,
        )
        self.assertEqual(
            tags_of(rsp),
            {
                "environment": "production",
                "team": "payments",
                "owner": "alice@example.com",
                "cost-center": "cc-4711",
            },
        )
        for_provider = rsp.desired.resources["bucket"].resource["spec"]["forProvider"]
        self.assertEqual(for_provider["region"], "eu-central-1")
        self.assertEqual(rsp.desired.resources["bucket"].resource["kind"], "Bucket")

    def test_the_cmdb_wins_over_a_tag_set_before_it_and_says_so(self):
        rsp = run_function(
            request(
                resources={
                    "bucket": bucket(tags={"owner": "mallory@example.com", "team": "payments"})
                }
            ),
            FakeCMDB(),
            log,
        )
        self.assertEqual(tags_of(rsp)["owner"], "alice@example.com")
        self.assertEqual(
            results_of(rsp),
            [
                (
                    SEVERITY_WARNING,
                    "bucket: replaced tag owner=mallory@example.com with the CMDB's owner=alice@example.com",
                ),
                (
                    SEVERITY_NORMAL,
                    "tagged 1 resource of team payments (owner alice@example.com, cost center cc-4711)",
                ),
            ],
        )

    def test_tags_every_managed_resource_and_leaves_the_rest_alone(self):
        config_map = {"apiVersion": "v1", "kind": "ConfigMap", "data": {"a": "b"}}
        rsp = run_function(
            request(resources={"bucket": bucket(), "logs": bucket(), "settings": config_map}),
            FakeCMDB(),
            log,
        )
        self.assertEqual(tags_of(rsp, "bucket")["team"], "payments")
        self.assertEqual(tags_of(rsp, "logs")["team"], "payments")
        settings = rsp.desired.resources["settings"].resource
        self.assertEqual(settings, req_resource(config_map))
        self.assertEqual(
            results_of(rsp)[-1][1],
            "tagged 2 resources of team payments (owner alice@example.com, cost center cc-4711)",
        )

    def test_writes_the_record_to_the_composite_status(self):
        req = request()
        req.desired.composite.resource.update({"status": {"other": "kept"}})
        rsp = run_function(req, FakeCMDB(), log)
        status = rsp.desired.composite.resource["status"]
        self.assertEqual(status["other"], "kept")
        self.assertEqual(
            dict(status["cmdb"].items()),
            {"owner": "alice@example.com", "costCenter": "cc-4711"},
        )


def req_resource(resource):
    req = RunFunctionRequest()
    req.desired.resources["x"].resource.update(resource)
    return req.desired.resources["x"].resource


class TestFatal(unittest.TestCase):
    """Every path that must not tag anything: run_function raises, and
    handle() turns the message into a fatal result."""

    def assert_fatal(self, req, message, cmdb=None):
        cmdb = cmdb or FakeCMDB()
        with self.assertRaises(RunError) as ctx:
            run_function(req, cmdb, log)
        self.assertEqual(str(ctx.exception), message)
        return cmdb

    def test_no_credential(self):
        cmdb = self.assert_fatal(
            request(tokens=None),
            'cannot look up team payments: the request carries no credential "cmdb"; '
            "declare it on the pipeline step",
        )
        self.assertEqual(cmdb.requests, [])

    def test_credential_without_data(self):
        req = request(tokens=None)
        req.credentials["cmdb"].SetInParent()
        self.assert_fatal(req, 'cannot look up team payments: credential "cmdb" has no data')

    def test_no_token_for_the_namespace(self):
        message = (
            'cannot look up team payments: credential "cmdb" has no token for namespace payments'
        )
        for tokens in ({"search": b"search-token"}, {"payments": b""}, {"payments": b"\n"}):
            with self.subTest(tokens=tokens):
                cmdb = self.assert_fatal(request(tokens=tokens), message)
                self.assertEqual(cmdb.requests, [])

    def test_token_not_utf8(self):
        self.assert_fatal(
            request(tokens={"payments": b"\xff"}),
            'cannot look up team payments: credential "cmdb" has a token for namespace '
            "payments that is not UTF-8",
        )

    def test_unknown_team(self):
        self.assert_fatal(
            request(namespace="ghosts", tokens={"ghosts": b"t"}),
            f"team ghosts is not in the CMDB: GET {CMDB}/teams/ghosts: status 404",
        )

    def test_failed_request(self):
        self.assert_fatal(
            request(),
            f"cannot look up team payments: GET {CMDB}/teams/payments: "
            "internal-error: sandbox.egress: no rule admits host",
            cmdb=FakeCMDB(error=ValueError("internal-error: sandbox.egress: no rule admits host")),
        )

    def test_server_error(self):
        self.assert_fatal(
            request(),
            f"cannot look up team payments: GET {CMDB}/teams/payments: status 503",
            cmdb=FakeCMDB(status=503),
        )

    def test_token_refused(self):
        self.assert_fatal(
            request(),
            f"cannot look up team payments: GET {CMDB}/teams/payments: status 401",
            cmdb=FakeCMDB(status=401),
        )

    def test_answer_not_json(self):
        with self.assertRaises(RunError) as ctx:
            run_function(request(), FakeCMDB(status=200, body=b"<html>"), log)
        self.assertTrue(
            str(ctx.exception).startswith(
                "cannot look up team payments: the CMDB's answer is not JSON: "
            ),
            str(ctx.exception),
        )

    def test_answer_not_an_object(self):
        self.assert_fatal(
            request(),
            "cannot look up team payments: the CMDB's answer is not an object",
            cmdb=FakeCMDB(status=200, body=b"[]"),
        )

    def test_answer_for_another_team(self):
        self.assert_fatal(
            request(),
            'cannot look up team payments: the CMDB\'s record is for team "search"',
            cmdb=FakeCMDB(records={"payments": SEARCH}),
        )

    def test_record_without_owner_or_cost_center(self):
        for key in ("owner", "costCenter"):
            with self.subTest(key=key):
                self.assert_fatal(
                    request(),
                    f"cannot look up team payments: the CMDB's record has no {key}",
                    cmdb=FakeCMDB(records={"payments": dict(PAYMENTS, **{key: ""})}),
                )

    def test_tags_not_an_object(self):
        resource = bucket()
        resource["spec"]["forProvider"]["tags"] = "team=payments"
        self.assert_fatal(
            request(resources={"bucket": resource}),
            "cannot tag bucket: spec.forProvider.tags is not an object",
        )

    def test_no_cmdb_url(self):
        self.assert_fatal(
            request(config={}), "cannot look up team payments: config.cmdbUrl is not set"
        )

    def test_cmdb_url_not_a_string(self):
        self.assert_fatal(
            request(config={"cmdbUrl": 7}), "cannot read config: cmdbUrl must be a string"
        )

    def test_no_namespace(self):
        cmdb = self.assert_fatal(
            request(namespace=None),
            "cannot read metadata: invoices has no namespace, which names its team",
        )
        self.assertEqual(cmdb.requests, [])

    def test_no_composite(self):
        self.assert_fatal(
            RunFunctionRequest(), "cannot get observed composite resource: none in request"
        )


class TestHandle(unittest.TestCase):
    def test_round_trip(self):
        out = handle(request().SerializeToString(), FakeCMDB(), log)
        rsp = RunFunctionResponse()
        rsp.ParseFromString(out)
        self.assertEqual(tags_of(rsp)["cost-center"], "cc-4711")

    def test_failure_is_a_fatal_result(self):
        out = handle(request(tokens={}).SerializeToString(), FakeCMDB(), log)
        rsp = RunFunctionResponse()
        rsp.ParseFromString(out)
        self.assertEqual(rsp.meta.tag, "t")
        self.assertEqual(
            results_of(rsp),
            [
                (
                    SEVERITY_FATAL,
                    'cannot look up team payments: credential "cmdb" has no token for namespace payments',
                )
            ],
        )
        # A fatal result composes nothing: Crossplane keeps what it has.
        self.assertFalse(rsp.HasField("desired"))

    def test_undecodable_request(self):
        rsp = RunFunctionResponse()
        rsp.ParseFromString(handle(b"\xff", FakeCMDB(), log))
        self.assertEqual(rsp.results[0].severity, SEVERITY_FATAL)
        self.assertTrue(rsp.results[0].message.startswith("cannot decode RunFunctionRequest: "))


if __name__ == "__main__":
    unittest.main()
