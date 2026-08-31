import hashlib
import http.client
import io
import json
import unittest
import urllib.error
import urllib.parse

from coop import Coop, CoopError
from rookhold import (
    Limits,
    Rookhold,
    RookholdError,
    _SameOriginRedirect,
    isolation_satisfies,
)


class CompatibilityTests(unittest.TestCase):
    def test_legacy_client_names_alias_rookhold(self):
        self.assertIs(Coop, Rookhold)
        self.assertIs(CoopError, RookholdError)


class Response:
    def __init__(self, value, status=200, headers=None, *, raw=None, url=None):
        self.body = (
            raw
            if raw is not None
            else json.dumps(value).encode()
            if value is not None
            else b""
        )
        self.status = status
        self.headers = headers or {}
        self.url = url

    def __enter__(self):
        return self

    def __exit__(self, *_args):
        return None

    def read(self):
        return self.body

    def geturl(self):
        return self.url


class QueueOpener:
    def __init__(self, *values):
        self.values = list(values)
        self.requests = []
        self.request_kwargs = []

    def __call__(self, request, **kwargs):
        self.requests.append(request)
        self.request_kwargs.append(kwargs)
        value = self.values.pop(0)
        if isinstance(value, Exception):
            raise value
        if isinstance(value, Response):
            return value
        return Response(value)


class Socket:
    def __init__(self, messages):
        self.messages = iter(messages)
        self.closed = False

    def recv(self):
        return next(self.messages, "")

    def close(self):
        self.closed = True


class CoopTests(unittest.TestCase):
    def test_isolation_satisfaction_matches_the_server_lattice(self):
        self.assertTrue(
            isolation_satisfies("gvisor-application-kernel", "linux-shared-kernel")
        )
        self.assertTrue(isolation_satisfies("confidential-vm", "hardware-vm"))
        self.assertFalse(
            isolation_satisfies("linux-shared-kernel", "gvisor-application-kernel")
        )
        self.assertTrue(isolation_satisfies("wasm-capability", "wasm-capability"))
        self.assertFalse(isolation_satisfies("hardware-vm", "wasm-capability"))
        self.assertFalse(isolation_satisfies("wasm-capability", "linux-shared-kernel"))

    def test_truncated_response_is_a_retryable_transport_error(self):
        class TruncatedResponse(Response):
            def __init__(self):
                super().__init__(None)

            def read(self):
                raise http.client.IncompleteRead(b'{"partial":', 128)

        def opener(_request, **_kwargs):
            return TruncatedResponse()

        client = Rookhold("https://example.test", "secret", opener=opener)
        with self.assertRaises(RookholdError) as raised:
            client.jobs()

        self.assertEqual(raised.exception.code, "transport_error")
        self.assertTrue(raised.exception.retryable)

    def test_truncated_error_response_is_retryable_and_closed(self):
        class TruncatedErrorBody(io.BytesIO):
            def read(self, *_args, **_kwargs):
                raise http.client.IncompleteRead(b'{"error":', 128)

        body = TruncatedErrorBody()
        error = urllib.error.HTTPError(
            "https://example.test/v1/jobs",
            503,
            "",
            {"Retry-After": "2", "x-request-id": "req-truncated"},
            body,
        )
        client = Rookhold("https://example.test", "secret", opener=QueueOpener(error))

        with self.assertRaises(RookholdError) as raised:
            client.jobs()

        self.assertEqual(raised.exception.status, 503)
        self.assertEqual(raised.exception.code, "transport_error")
        self.assertEqual(raised.exception.request_id, "req-truncated")
        self.assertEqual(raised.exception.retry_after, 2)
        self.assertTrue(raised.exception.retryable)
        self.assertTrue(error.closed)
        self.assertTrue(body.closed)

    def test_submit_serializes_explicit_limits_without_double_nesting(self):
        opener = QueueOpener(
            {"job_id": "j", "status": "queued", "stream_url": "/s", "replay_url": "/r"}
        )
        client = Rookhold("https://example.test/prefix/", "secret", opener=opener)
        client.submit("python", "print(1)", limits={"mem_mb": 128})
        body = json.loads(opener.requests[0].data)
        self.assertEqual(body["limits"], {"mem_mb": 128})
        self.assertEqual(
            opener.requests[0].full_url, "https://example.test/prefix/v1/jobs"
        )

    def test_submit_supports_dataclass_and_convenience_keywords(self):
        opener = QueueOpener(
            {"job_id": "j", "status": "queued", "stream_url": "/s", "replay_url": "/r"}
        )
        client = Rookhold("https://example.test", "secret", opener=opener)
        client.submit("bash", "true", limits=Limits(mem_mb=64), wall_seconds=3)
        body = json.loads(opener.requests[0].data)
        self.assertEqual(body["limits"], {"mem_mb": 64, "wall_seconds": 3})

    def test_submit_serializes_typed_atomic_execution_requirements(self):
        opener = QueueOpener(
            {"job_id": "j", "status": "queued", "stream_url": "/s", "replay_url": "/r"}
        )
        client = Rookhold("https://example.test", "secret", opener=opener)
        client.submit(
            "python",
            "pass",
            requirements={"minimum_isolation": "linux-shared-kernel"},
        )
        body = json.loads(opener.requests[0].data)
        self.assertEqual(
            body["requirements"], {"minimum_isolation": "linux-shared-kernel"}
        )

    def test_submit_accepts_empty_requirements_as_the_server_default(self):
        opener = QueueOpener(
            {"job_id": "j", "status": "queued", "stream_url": "/s", "replay_url": "/r"}
        )
        client = Rookhold("https://example.test", "secret", opener=opener)
        client.submit("python", "pass", requirements={})
        self.assertEqual(json.loads(opener.requests[0].data)["requirements"], {})

    def test_submit_uses_one_idempotency_key_for_ambiguous_retries(self):
        opener = QueueOpener(
            urllib.error.URLError(TimeoutError("response was ambiguous")),
            {
                "job_id": "j",
                "status": "queued",
                "stream_url": "/s",
                "replay_url": "/r",
            },
        )
        client = Rookhold("https://example.test", "secret", opener=opener)

        submitted = client.submit(
            "python",
            "print(1)",
            idempotency_key="submit-123",
            retry_ambiguous=True,
            retry_backoff=0,
        )

        self.assertEqual(submitted["job_id"], "j")
        self.assertEqual(len(opener.requests), 2)
        self.assertEqual(opener.requests[0].data, opener.requests[1].data)
        for request in opener.requests:
            self.assertEqual(request.get_header("Idempotency-key"), "submit-123")

    def test_submit_result_exposes_location_and_replay_metadata(self):
        body = {
            "job_id": "j",
            "status": "queued",
            "stream_url": "/s",
            "replay_url": "/r",
        }
        opener = QueueOpener(
            Response(
                body,
                status=201,
                headers={
                    "Location": "/v1/jobs/j",
                    "Idempotency-Replayed": "false",
                },
            ),
            Response(
                body,
                status=201,
                headers={
                    "Location": "/v1/jobs/j",
                    "Idempotency-Replayed": "true",
                },
            ),
        )
        client = Rookhold("https://example.test", "secret", opener=opener)
        first = client.submit_result("python", "pass", idempotency_key="submit-1")
        replay = client.submit_result("python", "pass", idempotency_key="submit-1")
        self.assertEqual(first["job"]["job_id"], "j")
        self.assertEqual(first["location"], "/v1/jobs/j")
        self.assertFalse(first["idempotency_replayed"])
        self.assertEqual(replay["job"]["job_id"], "j")
        self.assertTrue(replay["idempotency_replayed"])

    def test_submit_does_not_retry_ambiguity_without_explicit_opt_in(self):
        opener = QueueOpener(
            urllib.error.URLError(TimeoutError("response was ambiguous"))
        )
        client = Rookhold("https://example.test", "secret", opener=opener)

        with self.assertRaises(RookholdError) as raised:
            client.submit("python", "pass", idempotency_key="submit-456")

        self.assertEqual(raised.exception.code, "request_timeout")
        self.assertEqual(raised.exception.idempotency_key, "submit-456")
        self.assertEqual(len(opener.requests), 1)

    def test_unkeyed_ambiguous_submit_failure_is_not_safe_to_retry(self):
        opener = QueueOpener(
            urllib.error.URLError(TimeoutError("response was ambiguous"))
        )
        client = Rookhold("https://example.test", "secret", opener=opener)
        with self.assertRaises(RookholdError) as raised:
            client.submit("python", "pass")
        self.assertFalse(raised.exception.retryable)
        self.assertIsNone(raised.exception.idempotency_key)

    def test_submit_rejects_unsafe_retry_configuration_before_transport(self):
        client = Rookhold("https://example.test", "secret", opener=QueueOpener())
        for operation in (
            lambda: client.submit("python", "pass", idempotency_key="bad\nkey"),
            lambda: client.submit("python", "pass", idempotency_key="k" * 129),
            lambda: client.submit(
                "python", "pass", idempotency_key="key", max_ambiguous_retries=11
            ),
        ):
            with self.assertRaises(ValueError):
                operation()
        with self.assertRaises(TypeError):
            client.submit("python", "pass", retry_ambiguous=1)  # type: ignore[arg-type]

    def test_submit_generates_one_stable_key_for_opt_in_ambiguous_retry(self):
        opener = QueueOpener(
            urllib.error.URLError(TimeoutError("response was ambiguous")),
            {
                "job_id": "j",
                "status": "queued",
                "stream_url": "/s",
                "replay_url": "/r",
            },
        )
        client = Rookhold("https://example.test", "secret", opener=opener)
        client.submit("python", "pass", retry_ambiguous=True, retry_backoff=0)
        keys = [request.get_header("Idempotency-key") for request in opener.requests]
        self.assertEqual(len(set(keys)), 1)
        self.assertTrue(keys[0])

    def test_keyed_submit_refuses_redirects_that_can_change_the_request(self):
        request = urllib.request.Request(
            "https://example.test/v1/jobs",
            data=b"{}",
            method="POST",
            headers={"Idempotency-Key": "submit-1"},
        )
        handler = _SameOriginRedirect("https://example.test")
        with self.assertRaises(RookholdError) as raised:
            handler.redirect_request(
                request,
                None,
                302,
                "Found",
                {},
                "https://example.test/moved",
            )
        self.assertEqual(raised.exception.code, "unsafe_redirect")

    def test_authenticated_reads_refuse_cross_origin_redirects(self):
        request = urllib.request.Request(
            "https://example.test/v1/jobs/job/attestation",
            method="GET",
            headers={"Authorization": "Bearer tenant-secret"},
        )
        handler = _SameOriginRedirect("https://example.test")
        with self.assertRaises(RookholdError) as raised:
            handler.redirect_request(
                request,
                None,
                302,
                "Found",
                {},
                "https://attacker.example/evidence",
            )
        self.assertEqual(raised.exception.code, "unsafe_redirect")
        self.assertNotIn("tenant-secret", str(raised.exception))

    def test_structured_error_exposes_contract_fields(self):
        payload = {
            "error": {
                "code": "queue_full",
                "message": "busy",
                "request_id": "req-7",
                "retryable": True,
            }
        }
        error = urllib.error.HTTPError(
            "https://example.test/v1/jobs",
            503,
            "",
            {"Retry-After": "2", "x-request-id": "req-h"},
            io.BytesIO(json.dumps(payload).encode()),
        )
        client = Rookhold("https://example.test", "secret", opener=QueueOpener(error))
        with self.assertRaises(RookholdError) as raised:
            client.jobs()
        self.assertEqual(raised.exception.status, 503)
        self.assertEqual(raised.exception.code, "queue_full")
        self.assertEqual(raised.exception.request_id, "req-7")
        self.assertEqual(raised.exception.retry_after, 2)
        self.assertTrue(raised.exception.retryable)

    def test_structured_result_404_is_not_treated_as_a_legacy_route(self):
        payload = {
            "error": {
                "code": "job_not_found",
                "message": "job does not exist",
                "request_id": "req-result",
                "retryable": False,
            }
        }
        error = urllib.error.HTTPError(
            "https://example.test/v1/jobs/missing/result",
            404,
            "",
            {},
            io.BytesIO(json.dumps(payload).encode()),
        )
        opener = QueueOpener(error)
        client = Rookhold("https://example.test", "secret", opener=opener)

        with self.assertRaises(RookholdError) as raised:
            client.result("missing", timeout=1)

        self.assertEqual(raised.exception.code, "job_not_found")
        self.assertEqual(len(opener.requests), 1)

    def test_unstructured_result_404_uses_the_legacy_polling_fallback(self):
        error = urllib.error.HTTPError(
            "https://example.test/v1/jobs/j/result",
            404,
            "",
            {},
            io.BytesIO(b"not found"),
        )
        opener = QueueOpener(
            error,
            {
                "job_id": "j",
                "tenant": "acme",
                "language": "python",
                "status": "succeeded",
                "created_at_ms": 1,
                "started_at_ms": 2,
                "finished_at_ms": 3,
                "exit_code": 0,
            },
            {
                "events": [
                    {
                        "seq": 1,
                        "ts_ms": 2,
                        "kind": "stdout",
                        "data": {"line": "ok"},
                    }
                ],
                "next_cursor": None,
            },
        )
        result = Rookhold("https://example.test", "secret", opener=opener).result(
            "j", timeout=1
        )
        self.assertEqual(result["stdout"], "ok")
        self.assertEqual(len(opener.requests), 3)

    def test_wait_and_result_deadlines_are_finite_and_zero_is_immediate(self):
        opener = QueueOpener()
        client = Rookhold("https://example.test", "secret", opener=opener)
        for operation in (
            lambda: client.wait("j", timeout=0),
            lambda: client.result("j", timeout=0),
        ):
            with self.assertRaises(TimeoutError):
                operation()
        for operation in (
            lambda: client.wait("j", timeout=float("nan")),
            lambda: client.result("j", timeout=float("inf")),
        ):
            with self.assertRaises(ValueError):
                operation()
        self.assertEqual(opener.requests, [])

    def test_wait_and_result_cap_transport_timeout_to_remaining_budget(self):
        terminal = {"job_id": "j", "status": "succeeded"}
        result = {
            "job_id": "j",
            "status": "succeeded",
            "exit_code": 0,
            "duration_ms": 1,
            "stdout": "",
            "stderr": "",
            "truncated": False,
            "violations": [],
        }
        opener = QueueOpener(terminal, result)
        client = Rookhold("https://example.test", "secret", opener=opener)
        client.wait("j", timeout=0.25)
        client.result("j", timeout=0.25)
        timeouts = [call["timeout"] for call in opener.request_kwargs]
        self.assertTrue(all(0 < timeout <= 0.25 for timeout in timeouts))

    def test_list_and_event_envelopes_include_cursors_and_filters(self):
        opener = QueueOpener(
            {"items": [{"job_id": "j", "status": "running"}], "next_cursor": "opaque"},
            {
                "events": [{"seq": 4, "kind": "stdout", "data": {"line": "x"}}],
                "next_cursor": 4,
            },
        )
        client = Rookhold("https://example.test", "secret", opener=opener)
        page = client.list(
            limit=10, cursor="before", status="running", language="python"
        )
        events = client.event_page("job/id", after=3, limit=20)
        self.assertEqual(page["next_cursor"], "opaque")
        self.assertEqual(events["next_cursor"], 4)
        list_query = urllib.parse.parse_qs(
            urllib.parse.urlsplit(opener.requests[0].full_url).query
        )
        event_url = urllib.parse.urlsplit(opener.requests[1].full_url)
        self.assertEqual(list_query["status"], ["running"])
        self.assertIn("job%2Fid", event_url.path)
        self.assertEqual(urllib.parse.parse_qs(event_url.query)["after"], ["3"])

    def test_legacy_array_replay_is_filtered_client_side(self):
        opener = QueueOpener([{"seq": 1}, {"seq": 2}])
        events = Rookhold("https://example.test", "secret", opener=opener).replay(
            "j", after=1
        )
        self.assertEqual([event["seq"] for event in events], [2])

    def test_legacy_minus_one_cursor_is_normalized_for_v02_servers(self):
        opener = QueueOpener([])
        client = Rookhold("https://example.test", "secret", opener=opener)
        client.event_page("j", after=-1)
        query = urllib.parse.parse_qs(
            urllib.parse.urlsplit(opener.requests[0].full_url).query
        )
        self.assertEqual(query["after"], ["0"])

    def test_replay_collects_all_cursor_pages(self):
        opener = QueueOpener(
            {"events": [{"seq": 1}], "next_cursor": 1},
            {"events": [{"seq": 2}], "next_cursor": None},
        )
        client = Rookhold("https://example.test", "secret", opener=opener)
        self.assertEqual([event["seq"] for event in client.replay("j")], [1, 2])
        self.assertEqual(
            urllib.parse.parse_qs(
                urllib.parse.urlsplit(opener.requests[1].full_url).query
            )["after"],
            ["1"],
        )

    def test_polling_stream_yields_the_whole_terminal_replay_page(self):
        opener = QueueOpener(
            {
                "events": [
                    {
                        "seq": 1,
                        "ts_ms": 1,
                        "kind": "finished",
                        "data": {"status": "succeeded"},
                    },
                    {
                        "seq": 2,
                        "ts_ms": 2,
                        "kind": "legacy_tail",
                        "data": {},
                    },
                ],
                "next_cursor": None,
            }
        )
        client = Rookhold("https://example.test", "secret", opener=opener)
        events = list(
            client.stream(
                "j",
                prefer_websocket=False,
                poll_interval=0.001,
            )
        )
        self.assertEqual([event["seq"] for event in events], [1, 2])

    def test_polling_stream_drains_backlog_before_terminal_status_check(self):
        class BacklogOpener:
            def __init__(self):
                self.status_requests = 0

            def __call__(self, request, **_kwargs):
                url = urllib.parse.urlsplit(request.full_url)
                if url.path.endswith("/replay"):
                    query = urllib.parse.parse_qs(url.query)
                    after = int(query.get("after", ["0"])[0])
                    last = min(3_002, after + 500)
                    events = [
                        {
                            "seq": seq,
                            "ts_ms": seq,
                            "kind": "finished" if seq == 3_002 else "stdout",
                            "data": (
                                {"status": "succeeded"}
                                if seq == 3_002
                                else {"line": str(seq)}
                            ),
                        }
                        for seq in range(after + 1, last + 1)
                    ]
                    return Response(
                        {
                            "events": events,
                            "next_cursor": events[-1]["seq"] if events else None,
                        }
                    )
                self.status_requests += 1
                return Response({"job_id": "j", "status": "succeeded"})

        opener = BacklogOpener()
        client = Rookhold("https://example.test", "secret", opener=opener)
        events = list(
            client.stream(
                "j",
                prefer_websocket=False,
                poll_interval=0.001,
            )
        )
        self.assertEqual(len(events), 3_002)
        self.assertEqual(events[-1]["kind"], "finished")
        self.assertEqual(opener.status_requests, 0)

    def test_terminal_status_is_followed_by_a_final_catch_up_replay(self):
        class RacingOpener:
            def __init__(self):
                self.replays = 0
                self.status_requests = 0

            def __call__(self, request, **_kwargs):
                if urllib.parse.urlsplit(request.full_url).path.endswith("/replay"):
                    self.replays += 1
                    if self.replays <= 5:
                        return Response({"events": [], "next_cursor": None})
                    return Response(
                        {
                            "events": [
                                {
                                    "seq": 1,
                                    "ts_ms": 1,
                                    "kind": "finished",
                                    "data": {"status": "succeeded"},
                                }
                            ],
                            "next_cursor": 1,
                        }
                    )
                self.status_requests += 1
                return Response({"job_id": "j", "status": "succeeded"})

        opener = RacingOpener()
        client = Rookhold("https://example.test", "secret", opener=opener)
        events = list(
            client.stream(
                "j",
                prefer_websocket=False,
                poll_interval=0.001,
            )
        )
        self.assertEqual([event["kind"] for event in events], ["finished"])
        self.assertEqual(opener.replays, 6)
        self.assertEqual(opener.status_requests, 1)

    def test_cancel_accepts_an_empty_success_response(self):
        opener = QueueOpener(None, None)
        client = Rookhold("https://example.test", "secret", opener=opener)
        self.assertIsNone(client.cancel("j"))
        self.assertEqual(
            client.cancel_result("j"),
            {
                "job": None,
                "cancellation_requested": True,
                "already_terminal": False,
            },
        )
        self.assertEqual(opener.requests[0].method, "DELETE")

    def test_cancel_normalizes_the_current_acknowledgement_envelope(self):
        job = {"job_id": "j", "status": "running"}
        opener = QueueOpener(
            {
                "job": job,
                "cancellation_requested": True,
                "already_terminal": False,
            },
            {
                "job": job,
                "cancellation_requested": True,
                "already_terminal": False,
            },
            {
                "job": {"job_id": "j", "status": "succeeded"},
                "cancellation_requested": False,
                "already_terminal": True,
            },
        )
        client = Rookhold("https://example.test", "secret", opener=opener)
        self.assertEqual(client.cancel_result("j")["job"], job)
        self.assertEqual(client.cancel("j"), job)
        terminal = client.cancel_result("j")
        self.assertFalse(terminal["cancellation_requested"])
        self.assertTrue(terminal["already_terminal"])

    def test_whoami_and_capabilities_are_typed_endpoints(self):
        opener = QueueOpener(
            {"tenant": "acme"},
            {
                "version": "0.2.0",
                "languages": ["python"],
                "execution": {
                    "backend": "gvisor",
                    "isolation_class": "gvisor-application-kernel",
                    "isolated": True,
                },
                "limits": {
                    "wall_seconds_max": 300,
                    "concurrent_mem_mb_max": 8192,
                },
                "features": {
                    "stream_tickets": True,
                    "signed_attestations": True,
                },
                "attestations": {
                    "enabled": True,
                    "algorithm": "Ed25519",
                    "envelope_format": "DSSE/in-toto Statement v1",
                    "key_id": "ed25519:abc",
                    "public_key_url": "/v1/attestation/public-key",
                },
            },
        )
        client = Rookhold("https://example.test", "secret", opener=opener)
        self.assertEqual(client.whoami()["tenant"], "acme")
        capabilities = client.capabilities()
        self.assertTrue(capabilities["features"]["stream_tickets"])
        self.assertEqual(
            capabilities["execution"]["isolation_class"],
            "gvisor-application-kernel",
        )
        self.assertEqual(capabilities["limits"]["concurrent_mem_mb_max"], 8192)
        self.assertTrue(capabilities["features"]["signed_attestations"])
        self.assertEqual(capabilities["attestations"]["algorithm"], "Ed25519")
        self.assertEqual(opener.requests[0].full_url, "https://example.test/v1/whoami")

    def test_attestation_public_key_is_typed_authenticated_discovery(self):
        key = {
            "algorithm": "Ed25519",
            "key_id": "ed25519:abc",
            "public_key_pem": "-----BEGIN PUBLIC KEY-----\nabc\n-----END PUBLIC KEY-----\n",
            "trust_notice": "Pin this key out of band.",
        }
        opener = QueueOpener(key)
        client = Rookhold("https://example.test/prefix", "tenant-secret", opener=opener)

        self.assertEqual(client.attestation_public_key(), key)
        request = opener.requests[0]
        self.assertEqual(
            request.full_url,
            "https://example.test/prefix/v1/attestation/public-key",
        )
        self.assertEqual(request.get_header("Authorization"), "Bearer tenant-secret")

    def test_attestation_downloads_preserve_binary_order_and_transport_metadata(self):
        envelope = bytes([0, 255, 1, 128, 10, 13, 123, 125])
        artifact = bytes([125, 123, 13, 10, 128, 1, 255, 0])

        def binary_response(content, content_type):
            return Response(
                None,
                raw=content,
                headers={
                    "Content-Type": content_type,
                    "Content-Length": str(len(content)),
                    "X-Content-Sha256": hashlib.sha256(content).hexdigest(),
                },
                url="https://example.test/v1/jobs/job%2Fone/evidence",
            )

        opener = QueueOpener(
            binary_response(envelope, "application/vnd.dsse.envelope.v1+json"),
            binary_response(artifact, "application/vnd.coop.execution-result.v1+json"),
        )
        client = Rookhold("https://example.test", "tenant-secret", opener=opener)

        downloaded_envelope = client.download_attestation("job/one")
        downloaded_artifact = client.download_result_artifact("job/one")

        self.assertEqual(downloaded_envelope["content"], envelope)
        self.assertEqual(list(downloaded_envelope["content"]), list(envelope))
        self.assertEqual(downloaded_envelope["content_length"], len(envelope))
        self.assertEqual(
            downloaded_envelope["content_type"],
            "application/vnd.dsse.envelope.v1+json",
        )
        self.assertEqual(
            downloaded_envelope["sha256"], hashlib.sha256(envelope).hexdigest()
        )
        self.assertEqual(downloaded_artifact["content"], artifact)
        self.assertEqual(
            opener.requests[0].full_url,
            "https://example.test/v1/jobs/job%2Fone/attestation",
        )
        self.assertEqual(
            opener.requests[1].full_url,
            "https://example.test/v1/jobs/job%2Fone/result-artifact",
        )
        self.assertEqual(
            opener.requests[0].get_header("Accept"),
            "application/vnd.dsse.envelope.v1+json",
        )
        for request in opener.requests:
            self.assertEqual(
                request.get_header("Authorization"), "Bearer tenant-secret"
            )
            self.assertNotIn("tenant-secret", request.full_url)

    def test_artifact_download_rejects_missing_malformed_and_mismatched_digests(self):
        content = b"exact bytes"
        cases = (
            ({"Content-Type": "application/octet-stream"}, "invalid_response"),
            (
                {
                    "Content-Type": "application/octet-stream",
                    "X-Content-Sha256": "not-a-digest",
                },
                "invalid_response",
            ),
            (
                {
                    "Content-Type": "application/octet-stream",
                    "X-Content-Sha256": "0" * 64,
                },
                "content_digest_mismatch",
            ),
        )
        for headers, expected_code in cases:
            with self.subTest(expected_code=expected_code, headers=headers):
                client = Rookhold(
                    "https://example.test",
                    "secret",
                    opener=QueueOpener(Response(None, raw=content, headers=headers)),
                )
                with self.assertRaises(RookholdError) as raised:
                    client.download_attestation("job")
                self.assertEqual(raised.exception.code, expected_code)

    def test_artifact_download_preserves_structured_404(self):
        body = io.BytesIO(
            json.dumps(
                {
                    "error": {
                        "code": "attestation_unavailable",
                        "message": "no signed attestation",
                        "request_id": "req-attest",
                        "retryable": False,
                    }
                }
            ).encode()
        )
        error = urllib.error.HTTPError(
            "https://example.test/v1/jobs/job/attestation",
            404,
            "Not Found",
            {"Content-Type": "application/json"},
            body,
        )
        client = Rookhold("https://example.test", "secret", opener=QueueOpener(error))

        with self.assertRaises(RookholdError) as raised:
            client.download_attestation("job")

        self.assertEqual(raised.exception.status, 404)
        self.assertEqual(raised.exception.code, "attestation_unavailable")
        self.assertEqual(raised.exception.request_id, "req-attest")
        self.assertFalse(raised.exception.retryable)

    def test_artifact_download_rejects_cross_origin_responses_without_secret_text(self):
        content = b"evidence"
        response = Response(
            None,
            raw=content,
            headers={
                "Content-Type": "application/octet-stream",
                "X-Content-Sha256": hashlib.sha256(content).hexdigest(),
            },
            url="https://attacker.example/evidence",
        )
        client = Rookhold(
            "https://example.test", "tenant-secret", opener=QueueOpener(response)
        )

        with self.assertRaises(RookholdError) as raised:
            client.download_result_artifact("job")

        self.assertEqual(raised.exception.code, "unsafe_redirect")
        self.assertNotIn("tenant-secret", str(raised.exception))
        self.assertNotIn("tenant-secret", raised.exception.body)

    def test_artifact_download_timeout_is_bounded_and_retryable(self):
        opener = QueueOpener(urllib.error.URLError(TimeoutError("deadline")))
        client = Rookhold("https://example.test", "secret", opener=opener)

        with self.assertRaises(RookholdError) as raised:
            client.download_result_artifact("job", timeout=0.25)

        self.assertEqual(raised.exception.code, "request_timeout")
        self.assertTrue(raised.exception.retryable)
        self.assertEqual(opener.request_kwargs[0]["timeout"], 0.25)
        for invalid in (0, float("nan"), float("inf")):
            with self.assertRaises(ValueError):
                client.download_attestation("job", timeout=invalid)

    def test_job_detail_distinguishes_unknown_policy_and_output_evidence(self):
        stored_spec = {
            "language": "python",
            "code": "print('ok')",
            "stdin": None,
            "limits": {
                "wall_seconds": 15,
                "cpu_seconds": 10,
                "mem_mb": 256,
                "max_pids": 128,
                "max_file_mb": 16,
                "allow_network": False,
            },
            "requirements": {"minimum_isolation": "linux-shared-kernel"},
        }
        enforcement = {
            "wall_seconds": True,
            "cpu_seconds": True,
            "mem_mb": True,
            "max_pids": True,
            "max_file_mb": True,
        }
        effective_spec = {
            **stored_spec,
            "limits": {
                **stored_spec["limits"],
                "allow_network": False,
            },
            "isolation_class": "linux-shared-kernel",
        }
        base = {
            "tenant": "acme",
            "language": "python",
            "created_at_ms": 1,
            "started_at_ms": None,
            "finished_at_ms": None,
            "exit_code": None,
            "requested_spec": stored_spec,
            "receipt_sha256": None,
        }
        unknown = {
            **base,
            "job_id": "queued",
            "status": "queued",
            "effective_spec": None,
            "execution_policy": {
                "sandbox": None,
                "isolation_class": None,
                "bootstrap_ready": None,
                "isolated": None,
                "seccomp": None,
                "network_allowed": None,
                "networking": None,
                "private_rootfs": None,
                "dedicated_bootstrap": None,
                "limit_enforcement": None,
                "runtime_version": None,
                "runtime_sha256": None,
                "rootfs_sha256": None,
                "config_sha256": None,
            },
            "receipt": None,
        }
        known = {
            **base,
            "job_id": "done",
            "status": "succeeded",
            "effective_spec": effective_spec,
            "execution_policy": {
                "sandbox": "namespaces+cgroups-v2+private-rootfs",
                "isolation_class": "linux-shared-kernel",
                "bootstrap_ready": True,
                "isolated": True,
                "seccomp": True,
                "network_allowed": False,
                "networking": "disabled",
                "private_rootfs": True,
                "dedicated_bootstrap": True,
                "limit_enforcement": enforcement,
                "runtime_version": "python 3",
                "runtime_sha256": "1" * 64,
                "rootfs_sha256": "2" * 64,
                "config_sha256": "3" * 64,
            },
            "receipt": {
                "version": 1,
                "job_id": "done",
                "outcome": "succeeded",
                "exit_code": 0,
                "finished_at_ms": 3,
                "duration_ms": 1,
                "event_chain": {
                    "version": 1,
                    "head": "a" * 64,
                    "events": 3,
                    "event_count": 3,
                    "verified_events": 3,
                    "legacy_events": 0,
                    "complete": True,
                },
                "receipt_sha256": "b" * 64,
                "backend": "namespaces+cgroups-v2+private-rootfs",
                "minimum_isolation": "linux-shared-kernel",
                "isolation_class": "linux-shared-kernel",
                "bootstrap_ready": True,
                "isolated": True,
                "seccomp": True,
                "network_allowed": False,
                "networking": "disabled",
                "private_rootfs": True,
                "dedicated_bootstrap": True,
                "runtime_version": "python 3",
                "runtime_sha256": "1" * 64,
                "rootfs_sha256": "2" * 64,
                "config_sha256": "3" * 64,
                "effective_limits": effective_spec["limits"],
                "limit_enforcement": enforcement,
                "output": {
                    "encoding": "utf8-event-lines-joined-by-lf-no-trailing-lf",
                    "stdout_bytes": 2,
                    "stderr_bytes": 0,
                    "stdout_sha256": "c" * 64,
                    "stderr_sha256": "d" * 64,
                    "truncated": False,
                },
                "resource_usage": {
                    "wall_time_ms": 1,
                    "cpu_time_usec": 2,
                    "memory_peak_bytes": 3,
                },
                "executor_output": {
                    "stdout": {
                        "bytes_seen": 2,
                        "bytes_offered_to_sink": 2,
                        "records_offered_to_sink": 1,
                        "raw_sha256": "e" * 64,
                        "executor_truncated": False,
                    },
                    "stderr": {
                        "bytes_seen": 0,
                        "bytes_offered_to_sink": 0,
                        "records_offered_to_sink": 0,
                        "raw_sha256": "f" * 64,
                        "executor_truncated": False,
                    },
                },
            },
        }
        recovered = {
            **base,
            "job_id": "recovered",
            "status": "error",
            "effective_spec": None,
            "execution_policy": unknown["execution_policy"],
            "receipt_sha256": "9" * 64,
            "receipt": {
                "version": 1,
                "job_id": "recovered",
                "outcome": "error",
                "exit_code": None,
                "finished_at_ms": 4,
                "duration_ms": 2,
                "terminal_reason": "server_restarted",
                "requested_limits": {"wall_seconds": 15},
                "event_chain": {
                    "version": 1,
                    "head": "8" * 64,
                    "events": 3,
                    "event_count": 3,
                    "verified_events": 3,
                    "legacy_events": 0,
                    "complete": True,
                },
                "receipt_sha256": "9" * 64,
            },
        }
        opener = QueueOpener(unknown, known, recovered)
        client = Rookhold("https://example.test", "secret", opener=opener)

        queued = client.get("queued")
        complete = client.get("done")
        restarted = client.get("recovered")

        self.assertIsNone(queued["effective_spec"])
        self.assertIsNone(queued["execution_policy"]["sandbox"])
        self.assertEqual(
            complete["effective_spec"]["isolation_class"], "linux-shared-kernel"
        )
        self.assertEqual(
            complete["receipt"]["minimum_isolation"], "linux-shared-kernel"
        )
        self.assertEqual(
            complete["receipt"]["output"]["encoding"],
            "utf8-event-lines-joined-by-lf-no-trailing-lf",
        )
        self.assertEqual(
            complete["receipt"]["executor_output"]["stdout"]["raw_sha256"],
            "e" * 64,
        )
        self.assertTrue(complete["receipt"]["private_rootfs"])
        self.assertTrue(complete["receipt"]["dedicated_bootstrap"])
        self.assertTrue(complete["receipt"]["bootstrap_ready"])
        self.assertEqual(complete["receipt"]["limit_enforcement"], enforcement)
        self.assertIsNone(restarted["effective_spec"])
        self.assertIsNone(restarted["execution_policy"]["bootstrap_ready"])
        self.assertNotIn("output", restarted["receipt"])
        self.assertNotIn("private_rootfs", restarted["receipt"])
        self.assertNotIn("dedicated_bootstrap", restarted["receipt"])
        self.assertEqual(restarted["receipt"]["requested_limits"], {"wall_seconds": 15})

    def test_limit_names_are_checked_before_transport(self):
        client = Rookhold("https://example.test", "secret", opener=QueueOpener())
        with self.assertRaises(TypeError):
            client.submit("python", "pass", limits={"memory": 1})
        with self.assertRaises(ValueError):
            client.submit("python", "pass", limits={"mem_mb": 1}, mem_mb=2)

    def test_websocket_stream_uses_ticket_filters_duplicates_and_closes(self):
        opener = QueueOpener(
            {"ticket": "one use", "stream_url": "/v1/jobs/j/stream", "expires_at_ms": 1}
        )
        socket = Socket(
            [
                json.dumps({"seq": 2, "kind": "stdout", "data": {"line": "old"}}),
                json.dumps({"seq": 3, "kind": "stdout", "data": {"line": "new"}}),
                json.dumps(
                    {"seq": 4, "kind": "finished", "data": {"status": "succeeded"}}
                ),
            ]
        )
        urls = []

        def connect(url, **_kwargs):
            urls.append(url)
            return socket

        client = Rookhold(
            "https://example.test/prefix",
            "secret",
            opener=opener,
            websocket_factory=connect,
        )
        events = list(client.stream("j", after=2))
        self.assertEqual([event["seq"] for event in events], [3, 4])
        query = urllib.parse.parse_qs(urllib.parse.urlsplit(urls[0]).query)
        self.assertEqual(query["ticket"], ["one use"])
        self.assertEqual(query["after"], ["2"])
        self.assertEqual(
            urllib.parse.urlsplit(urls[0]).path, "/prefix/v1/jobs/j/stream"
        )
        self.assertTrue(socket.closed)

    def test_structured_v02_ticket_404_never_places_a_key_in_a_url(self):
        payload = {
            "error": {
                "code": "job_not_found",
                "message": "job does not exist",
                "request_id": "req-404",
                "retryable": False,
            }
        }
        error = urllib.error.HTTPError(
            "https://example.test/v1/jobs/missing/stream-ticket",
            404,
            "",
            {},
            io.BytesIO(json.dumps(payload).encode()),
        )
        opener = QueueOpener(
            error,
            {
                "events": [
                    {
                        "seq": 1,
                        "ts_ms": 1,
                        "kind": "finished",
                        "data": {"status": "failed"},
                    }
                ],
                "next_cursor": None,
            },
        )
        socket_urls = []
        client = Rookhold(
            "https://example.test",
            "secret",
            opener=opener,
            websocket_factory=lambda url, **_kwargs: socket_urls.append(url),
        )

        events = list(
            client.stream(
                "missing",
                allow_legacy_query_key=True,
                poll_interval=0.001,
            )
        )

        self.assertEqual([event["kind"] for event in events], ["finished"])
        self.assertEqual(socket_urls, [])
        self.assertTrue(
            all("key=" not in request.full_url for request in opener.requests)
        )

    def test_legacy_query_key_requires_explicit_opt_in(self):
        def legacy_error():
            return urllib.error.HTTPError(
                "https://example.test/v1/jobs/j/stream-ticket",
                404,
                "",
                {},
                io.BytesIO(b"not found"),
            )

        default_opener = QueueOpener(
            legacy_error(),
            {
                "events": [
                    {
                        "seq": 1,
                        "ts_ms": 1,
                        "kind": "finished",
                        "data": {"status": "failed"},
                    }
                ],
                "next_cursor": None,
            },
        )
        default_socket_urls = []
        default_client = Rookhold(
            "https://example.test",
            "secret",
            opener=default_opener,
            websocket_factory=lambda url, **_kwargs: default_socket_urls.append(url),
        )
        list(default_client.stream("j", poll_interval=0.001))
        self.assertEqual(default_socket_urls, [])

        opted_in_opener = QueueOpener(legacy_error())
        opted_in_socket = Socket(
            [
                json.dumps(
                    {
                        "seq": 1,
                        "ts_ms": 1,
                        "kind": "finished",
                        "data": {"status": "failed"},
                    }
                )
            ]
        )
        opted_in_urls = []

        def connect(url, **_kwargs):
            opted_in_urls.append(url)
            return opted_in_socket

        opted_in_client = Rookhold(
            "https://example.test",
            "secret",
            opener=opted_in_opener,
            websocket_factory=connect,
        )
        list(
            opted_in_client.stream(
                "j",
                allow_legacy_query_key=True,
                poll_interval=0.001,
            )
        )
        query = urllib.parse.parse_qs(urllib.parse.urlsplit(opted_in_urls[0]).query)
        self.assertEqual(query["key"], ["secret"])

    def test_base_url_rejects_credential_and_query_confusion(self):
        for url in (
            "example.test",
            "https://user@example.test",
            "https://example.test?q=1",
        ):
            with self.assertRaises(ValueError):
                Rookhold(url, "secret")

        for timeout in (float("nan"), float("inf"), 0):
            with self.assertRaises(ValueError):
                Rookhold("https://example.test", "secret", timeout=timeout)

    def test_stream_rejects_non_finite_polling_before_transport(self):
        client = Rookhold("https://example.test", "secret", opener=QueueOpener())
        for interval in (float("nan"), float("inf")):
            with self.assertRaises(ValueError):
                list(
                    client.stream(
                        "j",
                        prefer_websocket=False,
                        poll_interval=interval,
                    )
                )


if __name__ == "__main__":
    unittest.main()
