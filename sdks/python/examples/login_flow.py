# File: ballista/sdks/python/examples/login_flow.py
"""Authenticated user journey: inline test accounts, login once per user, token reuse, purchase transaction."""

from common import Progress, client, parse_args, quick, report

from ballista_client import Feeder, Load, Plan, Scenario, Target, Threshold, Wait
from ballista_client.plan import feed, get, post, think, transaction

ACCOUNTS = [{"username": f"shopper{i:03d}", "password": f"pw-{i:03d}-secret"} for i in range(1, 51)]


def build(target: str) -> Plan:
    return Plan(
        name="login flow",
        target=Target(base_url=target, headers={"accept": "application/json"}),
        load=Load.users(("10s", 20), ("30s", 20), ("5s", 0)),
        scenarios=[
            Scenario(
                name="shopper",
                wait=Wait.uniform("300ms", "1s"),
                on_start=[
                    feed("accounts"),
                    post(
                        "/login",
                        name="login",
                        json={"username": "${accounts.username}", "password": "${accounts.password}"},
                        capture={"token": "json.token"},
                    ),
                ],
                steps=[
                    get("/items", name="list items", query={"q": "item"}, capture={"item_id": "json.items.3.id"}),
                    think("200ms", "600ms"),
                    *transaction(
                        "purchase",
                        get("/items/${item_id}", name="item detail"),
                        post(
                            "/cart",
                            name="add to cart",
                            headers={"authorization": "Bearer ${token}"},
                            json={"item": "${item_id}", "qty": 1, "request_id": "${uuid()}"},
                            expect=[201, 503],
                        ),
                    ),
                ],
            )
        ],
        feeders=[Feeder(name="accounts", rows=ACCOUNTS, mode="sequential")],
        thresholds=[
            Threshold(metric="p95", request="purchase", max=400),
            Threshold(metric="error_rate", request="login", max=0.0),
        ],
        percentiles=[75, 99.9],
    )


def main() -> int:
    args = parse_args(__doc__ or "")
    with client() as c:
        run = c.run(build(args.target), quick(args), labels={"suite": "login"}, wait=not args.no_wait, on_stats=Progress())
        return 0 if args.no_wait else report(run)


if __name__ == "__main__":
    raise SystemExit(main())
