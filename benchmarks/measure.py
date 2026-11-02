# File: ballista/benchmarks/measure.py
"""Runs a load tool, samples connections and memory mid-run, reports CPU seconds of the whole process tree."""
import json
import resource
import subprocess
import sys
import threading
import time

import psutil


def established_to(port: int) -> int:
    n = 0
    with open("/proc/net/tcp") as f:
        next(f)
        for line in f:
            parts = line.split()
            if parts[3] == "01" and parts[2].endswith(f":{port:04X}"):
                n += 1
    return n


def main() -> None:
    label, sample_at, cmd = sys.argv[1], float(sys.argv[2]), sys.argv[3:]
    proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
    sample: dict[str, float] = {}

    def probe() -> None:
        time.sleep(sample_at)
        try:
            root = psutil.Process(proc.pid)
            tree = [root, *root.children(recursive=True)]
            sample["rss_mb"] = sum(p.memory_info().rss for p in tree) / 2**20
            sample["conns"] = established_to(8080)
            sample["cpu_pct"] = sum(p.cpu_percent(interval=None) for p in tree)
            time.sleep(1.0)
            sample["cpu_pct"] = sum(p.cpu_percent(interval=None) for p in tree if p.is_running())
        except psutil.Error:
            pass

    t = threading.Thread(target=probe)
    t.start()
    _, err = proc.communicate()
    t.join()
    ru = resource.getrusage(resource.RUSAGE_CHILDREN)
    out = {"label": label, "exit": proc.returncode, "cpu_s": round(ru.ru_utime + ru.ru_stime, 1), **{k: round(v, 1) for k, v in sample.items()}}
    print(json.dumps(out))
    if proc.returncode not in (0, 1, 2):
        print(err[-2000:], file=sys.stderr)


if __name__ == "__main__":
    main()
