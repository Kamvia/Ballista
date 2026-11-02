# File: ballista/sdks/python/examples/page_load.py
"""Browser-like page loads: the document first, then its resources in parallel, timed as one page."""

from common import Progress, client, parse_args, quick, report

from ballista_client import Load, Plan, Scenario, Target, Threshold, Wait
from ballista_client.plan import batch, get, think


def build(target: str) -> Plan:
    return Plan(
        name="page load",
        target=Target(base_url=target),
        load=Load.users(("10s", 15), ("30s", 15)),
        scenarios=[
            Scenario(
                name="visitor",
                wait=Wait.uniform("1s", "3s"),
                steps=[
                    get("/items", name="catalog document", capture={"first": "json.items.0.id"}),
                    batch(
                        get("/items/${first}", name="hero item"),
                        get("/items/2", name="related 1"),
                        get("/items/3", name="related 2"),
                        get("/slow", name="recommendations"),
                        name="catalog_assets",
                        concurrency=4,
                    ),
                    think("500ms", "1500ms"),
                ],
            )
        ],
        thresholds=[Threshold(metric="p95", request="catalog_assets", max=400)],
    )


def main() -> int:
    args = parse_args(__doc__ or "")
    with client() as c:
        run = c.run(build(args.target), quick(args, "8s"), labels={"suite": "page"}, wait=not args.no_wait, on_stats=Progress())
        if args.no_wait:
            return 0
        page = c.get_stats(run.id).row("catalog_assets")
        if page is not None:
            print(f"  page assets p50={page.p50_ms:.1f}ms p95={page.p95_ms:.1f}ms over {page.count} pages")
        return report(run)


if __name__ == "__main__":
    raise SystemExit(main())
