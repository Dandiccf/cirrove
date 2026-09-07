"""Synthetic mounted-file application; JSON contains timings/counts, never paths."""
import concurrent.futures
import json
import os
from pathlib import Path
import sys
import threading
import time

root = Path(sys.argv[1])
chunk_size = 64 * 1024


def summary(values):
    values = sorted(values)
    return {"samples": len(values), "p50_ms": values[(len(values)-1)//2],
            "p95_ms": values[(len(values)-1)*95//100], "max_ms": values[-1]}


def read_file(name, size, byte):
    start = time.monotonic()
    latencies = []
    total = 0
    with open(root / name, "rb", buffering=0) as source:
        while True:
            before = time.monotonic()
            chunk = source.read(chunk_size)
            elapsed = (time.monotonic() - before) * 1000
            if not chunk:
                break
            if not latencies:
                first = (time.monotonic() - start) * 1000
            latencies.append(elapsed)
            assert chunk == byte * len(chunk)
            total += len(chunk)
    assert total == size
    return {"bytes": total, "first_open_read_ms": first,
            "elapsed_ms": (time.monotonic() - start) * 1000,
            "read_calls": summary(latencies)}


def publish(phase, result):
    memory = Path("/proc/self/status").read_text()
    result["process_peak_rss_kib"] = int(next(
        line.split()[1] for line in memory.splitlines() if line.startswith("VmHWM:")))
    print(json.dumps({"phase": phase, "application": result}), flush=True)
    assert sys.stdin.readline().strip() == "continue"


publish("preview", read_file("preview", 3_100_000, b"C"))
reopens = [read_file("preview", 3_100_000, b"C") for _ in range(3)]
publish("reopen", {"reads": reopens,
                   "first_open_reads": summary([r["first_open_read_ms"] for r in reopens])})

latencies = []
with open(root / "sparse", "rb", buffering=0) as source:
    for offset in [0, 128*1024*1024, 32*1024*1024, 64*1024*1024]:
        start = time.monotonic()
        data = os.pread(source.fileno(), 4096, offset)
        latencies.append((time.monotonic() - start)*1000)
        assert data == b"D"*4096
publish("sparse", {"bytes": 4*4096, "read_calls": summary(latencies)})

barrier = threading.Barrier(32)


def concurrent_reader(_):
    barrier.wait(timeout=10)
    start = time.monotonic()
    with open(root / "shared", "rb", buffering=0) as source:
        data = source.read(4096)
    assert data == b"E"*4096
    return (time.monotonic()-start)*1000


with concurrent.futures.ThreadPoolExecutor(max_workers=32) as executor:
    latencies = list(executor.map(concurrent_reader, range(32)))
publish("concurrent", {"readers": 32, "bytes": 32*4096,
                       "first_open_reads": summary(latencies)})
publish("sequential", read_file("file", 1024*1024*1024, b"A"))
