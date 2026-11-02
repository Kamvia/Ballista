# File: ballista/sdks/python/examples/plan_with_assets.py
"""Existing plan text plus assets sent as content: a script, test data and a file the script uploads.

Assets are attached explicitly by name; the server reports any file the plan still needs.
"""

from pathlib import Path

from common import Progress, client, parse_args, quick, report

from ballista_client import Bundle

HERE = Path(__file__).parent / "assets"

PLAN = """
name = "uploads"

[target]
base_url = "{target}"

[load]
mode = "users"
stages = [{{ duration = "5s", target = 5 }}, {{ duration = "20s", target = 5 }}]

[[scenario]]
name = "uploader"
script = "scenarios/upload.rhai"
wait = {{ between = ["200ms", "600ms"] }}

[[feeder]]
name = "notes"
path = "data/notes.csv"

[[threshold]]
metric = "p95"
request = "upload"
max = 300.0
"""


def main() -> int:
    args = parse_args(__doc__ or "")
    text = PLAN.format(target=args.target)
    with client() as c:
        needed = c.validate_plan(text).files
        print(f"plan needs: {', '.join(needed)}")
        bundle = (
            Bundle(text, labels={"suite": "assets"})
            .add_path("scenarios/upload.rhai", HERE / "upload.rhai")
            .add_text("data/notes.csv", "note\n" + "\n".join(f"note number {i}" for i in range(1, 101)) + "\n")
            .add_path("files/report.txt", HERE / "report.txt")
        )
        missing = set(needed) - set(bundle.files)
        if missing:
            print(f"missing assets: {sorted(missing)}")
            return 1
        run = c.run(bundle, quick(args), wait=not args.no_wait, on_stats=Progress())
        return 0 if args.no_wait else report(run)


if __name__ == "__main__":
    raise SystemExit(main())
