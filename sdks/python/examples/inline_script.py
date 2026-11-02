# File: ballista/sdks/python/examples/inline_script.py
"""Scripted scenario sent inline: WebSocket round trips and Socket.IO acknowledgements with custom checks.

The script travels inside the plan and runs on the load generator; nothing executes in this process.
"""

from common import Progress, client, parse_args, quick, report

from ballista_client import Load, Plan, Scenario, Target, Threshold, Wait

REALTIME = r"""
fn tasks() { #{ websocket: 1, socketio: 2 } }

fn websocket(step, res) {
    switch step {
        0 => ws("/ws", `hello from ${this.vu_id}`, "ws echo"),
        1 => { if !res.body.starts_with("echo:") { throw "unexpected websocket reply"; } }
    }
}

fn socketio(step, res) {
    switch step {
        0 => #{ socketio: "/socket.io/", emit: "echo", data: #{ user: this.vu_id, n: rand_int(1, 99) }, ack: true, name: "sio echo" },
        1 => {
            if res.json.user != this.vu_id { throw "ack does not match"; }
            #{ socketio: "/socket.io/", emit: "ping", wait: "pong", name: "sio ping" }
        }
    }
}
"""


def build(target: str) -> Plan:
    return Plan(
        name="realtime",
        target=Target(base_url=target),
        load=Load.users(("5s", 10), ("25s", 10)),
        scenarios=[Scenario(name="realtime", source=REALTIME, wait=Wait.fixed("200ms"))],
        thresholds=[Threshold(metric="p95", request="sio echo", max=50), Threshold(metric="error_rate", max=0.0)],
    )


def main() -> int:
    args = parse_args(__doc__ or "")
    with client() as c:
        plan = build(args.target)
        check = c.validate_plan(plan)
        print(f"plan '{check.name}' valid, scenarios: {', '.join(check.scenarios)}")
        run = c.run(plan, quick(args), labels={"suite": "realtime"}, wait=not args.no_wait, on_stats=Progress())
        return 0 if args.no_wait else report(run)


if __name__ == "__main__":
    raise SystemExit(main())
