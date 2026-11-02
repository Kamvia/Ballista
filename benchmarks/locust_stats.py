# File: ballista/benchmarks/locust_stats.py
import csv
import sys

for row in csv.DictReader(open(sys.argv[1])):
    if row["Name"] == "Aggregated":
        print(f"   requests={row['Request Count']} fails={row['Failure Count']} rps_avg={float(row['Requests/s']):.0f} p50={row['50%']}ms p95={row['95%']}ms p99={row['99%']}ms")
