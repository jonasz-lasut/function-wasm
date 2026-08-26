# Native tests for the guest's logic under plain python (unittest): the
# fetch and log doubles stand in for the world's imports, exactly as the
# other guests' native tests stub their hosts.

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent.parent / "src"))
sys.path.insert(0, str(Path(__file__).parent.parent / "src" / "gen"))

from fn import handle, run_function  # noqa: E402
from run_function_pb2 import (  # noqa: E402
    SEVERITY_FATAL,
    RunFunctionRequest,
    RunFunctionResponse,
)


def log(level, msg, kv):
    pass


def fake_fetch(url):
    if url == "https://greetings.example.com/en":
        return "howdy"
    raise ValueError(f'internal-error: sandbox.egress: no rule admits host "{url}"')


def request(config=None, desired=None):
    req = RunFunctionRequest()
    req.meta.tag = "hello"
    req.observed.composite.resource.update(
        {
            "apiVersion": "example.org/v1",
            "kind": "XR",
            "metadata": {"name": "my-xr"},
        }
    )
    if config is not None:
        req.input.update({"config": config})
    if desired is not None:
        for name in desired:
            req.desired.resources[name].SetInParent()
    return req


def greeting_of(rsp):
    data = rsp.desired.resources["greeting"].resource.fields["data"].struct_value
    return data.fields["greeting"].string_value


class TestRunFunction(unittest.TestCase):
    def test_default_greeting(self):
        rsp = run_function(request(), fake_fetch, log)
        self.assertEqual(greeting_of(rsp), "hello my-xr")
        self.assertEqual(rsp.meta.tag, "hello")
        self.assertEqual(rsp.results[0].message, "greeted my-xr")
        self.assertEqual(rsp.conditions[0].type, "FunctionSuccess")

    def test_configured_greeting_keeps_desired(self):
        rsp = run_function(request(config={"greeting": "hi"}, desired=["other"]), fake_fetch, log)
        self.assertEqual(greeting_of(rsp), "hi my-xr")
        self.assertIn("other", rsp.desired.resources)

    def test_bad_config_is_an_error(self):
        from fn import RunError

        with self.assertRaises(RunError) as ctx:
            run_function(request(config={"greeting": 7}), fake_fetch, log)
        self.assertEqual(str(ctx.exception), "cannot read config: greeting must be a string")

    def test_greeting_from_url_through_the_fetcher(self):
        rsp = run_function(
            request(config={"greetingUrl": "https://greetings.example.com/en"}), fake_fetch, log
        )
        self.assertEqual(greeting_of(rsp), "howdy my-xr")
        from fn import RunError

        with self.assertRaises(RunError) as ctx:
            run_function(
                request(config={"greetingUrl": "https://evil.example.com/en"}), fake_fetch, log
            )
        self.assertEqual(
            str(ctx.exception),
            'cannot fetch greeting: internal-error: sandbox.egress: no rule admits host "https://evil.example.com/en"',
        )

    def test_handle_round_trip_reports_fatal(self):
        req = RunFunctionRequest()
        req.meta.tag = "t"
        out = handle(req.SerializeToString(), fake_fetch, log)
        rsp = RunFunctionResponse()
        rsp.ParseFromString(out)
        self.assertEqual(rsp.meta.tag, "t")
        self.assertEqual(rsp.results[0].severity, SEVERITY_FATAL)
        self.assertEqual(
            rsp.results[0].message, "cannot get observed composite resource: none in request"
        )


if __name__ == "__main__":
    unittest.main()
