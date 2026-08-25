import json
import time
import urllib.error
import urllib.request

TERMINAL = {"succeeded", "failed", "timed_out", "oom_killed", "cancelled", "error"}


class CoopError(RuntimeError):
    def __init__(self, status, body):
        super().__init__(f"coop returned {status}: {body}")
        self.status = status
        self.body = body


class Coop:
    def __init__(self, base_url, api_key):
        self.base_url = base_url.rstrip("/")
        self.api_key = api_key

    def _request(self, method, path, payload=None):
        data = json.dumps(payload).encode() if payload is not None else None
        req = urllib.request.Request(
            self.base_url + path,
            data=data,
            method=method,
            headers={
                "Authorization": f"Bearer {self.api_key}",
                "Content-Type": "application/json",
            },
        )
        try:
            with urllib.request.urlopen(req) as resp:
                return json.loads(resp.read().decode() or "null")
        except urllib.error.HTTPError as e:
            raise CoopError(e.code, e.read().decode()) from None

    def submit(self, language, code, stdin=None, **limits):
        spec = {"language": language, "code": code}
        if stdin is not None:
            spec["stdin"] = stdin
        if limits:
            spec["limits"] = limits
        return self._request("POST", "/v1/jobs", spec)

    def get(self, job_id):
        return self._request("GET", f"/v1/jobs/{job_id}")

    def jobs(self, limit=50):
        return self._request("GET", f"/v1/jobs?limit={limit}")

    def replay(self, job_id):
        return self._request("GET", f"/v1/jobs/{job_id}/replay")

    def wait(self, job_id, timeout=60.0, poll=0.25):
        deadline = time.time() + timeout
        while time.time() < deadline:
            view = self.get(job_id)
            if view.get("status") in TERMINAL:
                return view
            time.sleep(poll)
        raise TimeoutError(f"job {job_id} still running after {timeout}s")

    def result(self, job_id, timeout=60.0):
        # One-call fast path: newer servers fold the event log into a flat
        # result server-side and wait for us. 202 (server budget expired,
        # still running) returns a partial view — keep long-polling until
        # terminal or our deadline, rather than handing back a non-terminal
        # result as if it were final (deep-hunt fix).
        deadline = time.time() + timeout
        while True:
            remaining = max(0, int(deadline - time.time()))
            try:
                view = self._request(
                    "GET", f"/v1/jobs/{job_id}/result?wait_seconds={remaining}"
                )
            except CoopError as e:
                if e.status not in (404, 405):
                    raise
                # Older server without /result: fall back to poll + replay.
                return self._result_via_polling(job_id, deadline)
            if view.get("status") in TERMINAL:
                return view
            if time.time() >= deadline:
                raise TimeoutError(f"job {job_id} still running after {timeout}s")
            time.sleep(0.25)

    def _result_via_polling(self, job_id, deadline):
        timeout = max(0.0, deadline - time.time())
        view = self.wait(job_id, timeout=timeout)
        stdout, stderr = [], []
        for event in self.replay(job_id):
            kind = event.get("kind")
            line = (event.get("data") or {}).get("line", "")
            if kind == "stdout":
                stdout.append(line)
            elif kind == "stderr":
                stderr.append(line)
        return {
            "status": view["status"],
            "exit_code": view.get("exit_code"),
            "stdout": "\n".join(stdout),
            "stderr": "\n".join(stderr),
        }

    def stream(self, job_id, cursor=-1):
        seq = cursor
        while True:
            for event in self.replay(job_id):
                if event["seq"] > seq:
                    seq = event["seq"]
                    yield event
            view = self.get(job_id)
            if view.get("status") in TERMINAL:
                return
            time.sleep(0.25)


if __name__ == "__main__":
    import os
    import sys

    base = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:7300"
    key = sys.argv[2] if len(sys.argv) > 2 else os.environ.get("COOP_API_KEY", "coop-dev-key")
    coop = Coop(base, key)
    job = coop.submit("python", "print('hello from the coop sdk')")
    print("submitted:", job["job_id"])
    print(coop.result(job["job_id"]))
