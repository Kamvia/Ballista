# File: ballista/benchmarks/report_stats.py
import json, sys
s = json.load(open(sys.argv[1]))["snapshot"]
t = s["total"]
print(f"   requests={t['count']} fails={t['failures']} rps_avg={t['rps_avg']:.0f} p95={t['p95_ms']:.1f}ms p99={t['p99_ms']:.1f}ms peak_users={max(p['users'] for p in s['timeline'])} saturated_s={s['generator']['saturated_secs']}")
for e in s["errors"][:3]:
    print("   error", e["message"], e["count"])
