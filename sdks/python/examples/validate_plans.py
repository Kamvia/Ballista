# File: ballista/sdks/python/examples/validate_plans.py
"""Dry run: let the server validate plans (structure, steps, allowed hosts, limits) without running them."""

from collections.abc import Callable

from common import client, parse_args

from ballista_client import Load, Plan, Scenario, Target, ValidationError
from ballista_client.bundle import PlanInput
from ballista_client.plan import get


def good(target: str) -> Plan:
    return Plan(name="valid", target=Target(base_url=target), load=Load.users(("10s", 5)), scenarios=[Scenario(name="a", steps=[get("/items")])])


CASES: dict[str, Callable[[str], PlanInput]] = {
    "foreign host": lambda t: good("http://10.255.255.1:9000/"),
    "too many users": lambda t: Plan(name="big", target=Target(base_url=t), load=Load.users(("10s", 1_000_000)), scenarios=[Scenario(name="a", steps=[get("/")])]),
    "unknown variable source": lambda t: {
        "target": {"base_url": t},
        "load": {"stages": [{"duration": "5s", "target": 1}]},
        "scenario": [{"name": "a", "step": [{"get": "/", "capture": {"x": "cookie.session"}}]}],
    },
    "missing asset (plan text)": lambda t: f'[target]\nbase_url = "{t}"\n[load]\nstages = [{{ duration = "5s", target = 1 }}]\n[[scenario]]\nname = "a"\nscript = "scenarios/a.rhai"\n',
}


def main() -> int:
    args = parse_args(__doc__ or "")
    failures = 0
    with client() as c:
        check = c.validate_plan(good(args.target))
        print(f"[ok]      valid plan '{check.name}', needs files: {check.files or 'none'}")
        for label, make in CASES.items():
            try:
                result = c.validate_plan(make(args.target))
                if result.files:
                    print(f"[files]   {label}: bundle must also carry {result.files}")
                else:
                    print(f"[UNEXPECTED] {label}: accepted")
                    failures += 1
            except ValidationError as e:
                print(f"[rejected] {label}: {e.message}")
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
