# File: ballista/sdks/python/examples/parallel_suites.py
"""Async client: queue several test suites at once and follow them concurrently.

The server executes one run at a time; the others wait in its queue.
"""

import asyncio

from common import parse_args

from ballista_client import AsyncClient, Load, Plan, Run, RunState, Scenario, Snapshot, Target, Threshold
from ballista_client.plan import get

SUITES = {
    "catalog": [get("/items", name="list items")],
    "detail": [get("/items/4", name="item detail")],
    "slow": [get("/slow", name="slow page")],
}


def build(target: str, name: str) -> Plan:
    return Plan(
        name=f"suite {name}",
        target=Target(base_url=target),
        load=Load.users(("2s", 5), ("6s", 5)),
        scenarios=[Scenario(name=name, steps=SUITES[name])],
        thresholds=[Threshold(metric="error_rate", max=0.0)],
    )


async def follow(c: AsyncClient, name: str, run: Run) -> Run:
    def tick(s: Snapshot) -> None:
        if int(s.elapsed_secs) % 2 == 0:
            print(f"  [{name}] t={s.elapsed_secs:.0f}s rps={s.total.rps:.1f}")

    done = await c.wait(run.id, timeout=600, on_stats=tick)
    print(f"[{name}] {done.state.value}")
    return done


async def main_async(target: str) -> int:
    async with AsyncClient(timeout=60.0) as c:
        uploads = await asyncio.gather(*(c.upload_bundle(build(target, n)) for n in SUITES))
        runs = [await c.create_run(info, labels={"suite": n}) for n, info in zip(SUITES, uploads, strict=True)]
        for n, r in zip(SUITES, runs, strict=True):
            print(f"[{n}] queued {r.id} at position {r.queue_position}")
        results = await asyncio.gather(*(follow(c, n, r) for n, r in zip(SUITES, runs, strict=True)))
    return 0 if all(r.state is RunState.PASSED for r in results) else 2


def main() -> int:
    args = parse_args(__doc__ or "")
    return asyncio.run(main_async(args.target))


if __name__ == "__main__":
    raise SystemExit(main())
