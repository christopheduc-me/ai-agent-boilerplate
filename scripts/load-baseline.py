#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = ["httpx>=0.28"]
# ///
"""Capacity baseline against the fake-provider stack (ADR-072).

ADR-070 and ADR-071 shipped numbers — the SSE poll cadence, the connection pool,
the worker concurrency — that were reasoned from the code and never measured.
Both ADRs say so. This measures the two that can be measured cheaply, because
`AGENT_PROVIDERS=fake` (ADR-021) makes the stack keyless and deterministic: it
can be loaded as hard as you like without paying an API bill or fighting
provider variance.

    docker compose --profile full up -d --build --wait   # with AGENT_PROVIDERS=fake
    uv run scripts/load-baseline.py

What it reports, and nothing more:

1. **Database cost of idle SSE viewers.** N streams are opened on jobs parked in
   `awaiting_input` — the state ADR-070 found unbounded — and the PostgreSQL
   commit counter is sampled to get the real transaction rate per viewer.
2. **Burst resilience.** M searches are submitted at once and every one is
   checked to come back. No throughput figure: fake providers return instantly,
   so there is no latency for `CELERY_CONCURRENCY` to parallelise — measuring it
   here would put a confident number on this script's polling granularity.

The numbers are only comparable against themselves: they come from whatever
machine runs them, under Docker, with fake providers. Treat them as a baseline
to re-measure after a change, never as a capacity promise for a real host.
"""

from __future__ import annotations

import argparse
import subprocess
import sys
import threading
import time
from dataclasses import dataclass

import httpx

BASE_URL = "http://localhost:8080"
PASSWORD = "load-b4seline-password"
# The keyword the fake policy answers with a clarification request, so the job
# parks in `awaiting_input` and its stream stays open (scripts/e2e-smoke.sh).
HITL_KEYWORD = "ambiguous smoke topic"


def pg_commits() -> int:
    """Committed transactions on the app database, straight from PostgreSQL —
    measuring at the source avoids trusting the app's own accounting."""
    out = subprocess.run(
        [
            "docker",
            "compose",
            "exec",
            "-T",
            "postgres",
            "psql",
            "-U",
            "app",
            "-d",
            "aiagent",
            "-tAc",
            "select xact_commit from pg_stat_database where datname='aiagent'",
        ],
        capture_output=True,
        text=True,
        check=True,
    )
    return int(out.stdout.strip())


@dataclass
class Session:
    client: httpx.Client
    token: str

    @property
    def auth(self) -> dict[str, str]:
        return {"authorization": f"Bearer {self.token}"}


def sign_up(client: httpx.Client) -> Session:
    email = f"load-{time.time_ns()}@test.dev"
    body = {"email": email, "password": PASSWORD}
    client.post("/api/auth/register", json=body).raise_for_status()
    login = client.post("/api/auth/login", json=body)
    login.raise_for_status()
    return Session(client, login.json()["access_token"])


def launch(session: Session, keyword: str, mode: str | None = None) -> str:
    body: dict[str, str] = {"keyword": keyword}
    if mode:
        body["mode"] = mode
    reply = session.client.post("/api/searches", json=body, headers=session.auth)
    reply.raise_for_status()
    return reply.json()["job_id"]


def wait_for_status(session: Session, job_id: str, wanted: set[str], timeout: float) -> str:
    deadline = time.monotonic() + timeout
    status = "unknown"
    while time.monotonic() < deadline:
        reply = session.client.get(f"/api/searches/{job_id}", headers=session.auth)
        reply.raise_for_status()
        status = reply.json()["status"]
        if status in wanted:
            return status
        time.sleep(0.5)
    return status


# --------------------------------------------------------------------- 1. SSE
def measure_sse(session: Session, viewers: int, window: float) -> None:
    print(f"\n== SSE cost: {viewers} idle viewers over {window:.0f}s")

    print("   parking jobs in awaiting_input ...", end="", flush=True)
    job_ids: list[str] = []
    for _ in range(viewers):
        job_id = launch(session, HITL_KEYWORD, mode="agent")
        if wait_for_status(session, job_id, {"awaiting_input"}, timeout=60) != "awaiting_input":
            sys.exit("\n   a job never reached awaiting_input — is AGENT_PROVIDERS=fake set?")
        job_ids.append(job_id)
    print(f" {len(job_ids)} parked")

    stop = threading.Event()
    opened = threading.Barrier(viewers + 1, timeout=60)

    def hold(job_id: str) -> None:
        # A separate client per stream: httpx pools connections, and sharing one
        # would serialise the streams instead of holding them open in parallel.
        with httpx.Client(base_url=BASE_URL, timeout=None) as client:
            with client.stream(
                "GET", f"/api/searches/{job_id}/events", headers=session.auth
            ) as stream:
                opened.wait()
                for _ in stream.iter_lines():
                    if stop.is_set():
                        return

    threads = [threading.Thread(target=hold, args=(j,), daemon=True) for j in job_ids]
    for thread in threads:
        thread.start()
    opened.wait()

    # Let the initial burst (every stream emits current state at once) settle,
    # so the sample covers the steady state rather than connection setup.
    time.sleep(3)

    before = pg_commits()
    time.sleep(window)
    after = pg_commits()
    stop.set()

    total = (after - before) / window
    print(f"   {total:8.1f} transactions/s total")
    print(f"   {total / viewers:8.2f} transactions/s per idle viewer")
    print("   (ADR-070 slowed awaiting_input polling to one read per 15s;")
    print("    at the old 1s cadence this would be ~3 per viewer per second)")


# ------------------------------------------------------------------- 2. burst
def measure_burst(session: Session, jobs: int) -> None:
    """Submits a burst and checks every job comes back.

    This deliberately reports **no jobs/min figure**. `CELERY_CONCURRENCY` only
    buys anything when workers block — on Anthropic, on Tavily — and the fake
    providers (ADR-021) return instantly, so there is no latency to parallelise.
    Measured that way the numbers swung between 1.2k and 12.9k "jobs/min" across
    runs of the same command: what varied was this script's own polling
    granularity, not the system. Worker concurrency cannot be characterised
    without real provider latency, and pretending otherwise would put a
    confident number on nothing.

    What the burst does prove: the submission path, the queue and the callback
    contract hold when several hundred jobs arrive at once.
    """
    print(f"\n== Burst: {jobs} searches submitted at once")

    start = time.monotonic()
    job_ids = [launch(session, f"load baseline {i}") for i in range(jobs)]
    submitted = time.monotonic() - start
    print(f"   {submitted:.2f}s to submit ({jobs / submitted:.0f} accepted/s)")

    done = sum(
        wait_for_status(session, job_id, {"completed", "failed"}, timeout=180) == "completed"
        for job_id in job_ids
    )
    print(f"   {done}/{jobs} completed in {time.monotonic() - start:.1f}s wall time")
    if done < jobs:
        print(f"   WARNING: {jobs - done} job(s) did not complete — the queue dropped work")
    print("   (no jobs/min figure on purpose: fake providers have no latency to")
    print("    parallelise, so this cannot measure CELERY_CONCURRENCY — see the docstring)")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--viewers", type=int, default=10, help="concurrent SSE streams")
    parser.add_argument("--jobs", type=int, default=200, help="searches in the burst")
    parser.add_argument("--window", type=float, default=30.0, help="SSE sampling window, seconds")
    args = parser.parse_args()

    with httpx.Client(base_url=BASE_URL, timeout=30.0) as client:
        try:
            client.get("/", timeout=5.0).raise_for_status()
        except httpx.HTTPError:
            sys.exit(f"stack unreachable at {BASE_URL} — boot it with --profile full first")
        session = sign_up(client)
        measure_sse(session, args.viewers, args.window)
        measure_burst(session, args.jobs)

    print("\n== baseline complete — comparable against itself, not a capacity promise")


if __name__ == "__main__":
    main()
