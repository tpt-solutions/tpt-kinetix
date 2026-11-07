#!/usr/bin/env python3
"""Load test of `tpt-kinetix live`: N concurrent publishers, M concurrent viewers.

Each publisher pushes the same pre-made WebM (stream copy, paced with `ffmpeg -re`)
to `/ingest/pub<i>`. Each viewer polls a media playlist of one stream as fast as
it can for the duration of the run. The server's resident memory is sampled once
a second. Output: a JSON summary on stdout (and in --out, if given).

Standard library only. Needs ffmpeg on PATH (and ffprobe is not needed).

  python tools/live-load-test.py --tpt target/release/tpt-kinetix \
      --publishers 4 --viewers 16 --seconds 20 --out docs/perf/live-load.json

The result is a measurement for a given machine, not a gate: record it next to
the hardware and the commit it was taken on.
"""

import argparse
import json
import os
import platform
import re
import socket
import statistics
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def make_source(path, seconds):
    subprocess.run(
        [
            "ffmpeg", "-v", "error", "-y", "-f", "lavfi", "-i",
            "testsrc=size=320x180:rate=25", "-t", str(seconds), "-g", "50",
            "-c:v", "libvpx-vp9", "-deadline", "realtime", "-cpu-used", "8",
            "-b:v", "300k", path,
        ],
        check=True,
    )


def rss_kib(pid):
    """Resident set size of `pid` in KiB, or None if it cannot be read."""
    if platform.system() == "Windows":
        out = subprocess.run(
            ["tasklist", "/FI", f"PID eq {pid}", "/FO", "CSV", "/NH"],
            capture_output=True, text=True,
        ).stdout
        m = re.search(r'"([\d,.]+) K"', out)
        return int(m.group(1).replace(",", "").replace(".", "")) if m else None
    try:
        with open(f"/proc/{pid}/status") as f:
            for line in f:
                if line.startswith("VmRSS:"):
                    return int(line.split()[1])
    except OSError:
        return None
    return None


def cpu_seconds(pid):
    """Total CPU time (user + system) of `pid` in seconds, or None if unreadable."""
    if platform.system() == "Windows":
        out = subprocess.run(
            [
                "powershell", "-NoProfile", "-Command",
                f"[string]::Format([Globalization.CultureInfo]::InvariantCulture, '{{0}}', (Get-Process -Id {pid}).TotalProcessorTime.TotalSeconds)",
            ],
            capture_output=True, text=True,
        ).stdout.strip()
        try:
            return float(out)
        except ValueError:
            return None
    try:
        with open(f"/proc/{pid}/stat") as f:
            fields = f.read().rsplit(")", 1)[1].split()
        ticks = int(fields[11]) + int(fields[12])  # utime, stime
        return ticks / os.sysconf("SC_CLK_TCK")
    except (OSError, IndexError, ValueError):
        return None


def start_cpu_probe(exe, seconds):
    """Windows: a typeperf process sampling the server's processor-time counter
    once a second. (TotalProcessorTime moves in 15.6 ms steps, too coarse for a
    server that uses well under one core, so the counter is read instead.)"""
    if platform.system() != "Windows":
        return None
    name = os.path.splitext(os.path.basename(exe))[0]
    return subprocess.Popen(
        ["typeperf", f"\\Process({name})\\% Processor Time", "-sc", str(seconds), "-si", "1"],
        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
    )


def read_cpu_probe(proc):
    """Mean cores used over the samples, or None."""
    if proc is None:
        return None
    out, _ = proc.communicate(timeout=120)
    values = []
    for line in out.splitlines():
        parts = line.rsplit(",", 1)
        if len(parts) != 2:
            continue
        text = parts[1].strip().strip('"')
        try:
            values.append(float(text))
        except ValueError:
            continue  # the header lines
    return sum(values) / len(values) / 100.0 if values else None


def percentile(sorted_values, p):
    if not sorted_values:
        return None
    k = min(len(sorted_values) - 1, max(0, round(p / 100 * (len(sorted_values) - 1))))
    return sorted_values[k]


class Viewer(threading.Thread):
    def __init__(self, base, stream, deadline):
        super().__init__(daemon=True)
        self.url = f"{base}/{stream}/track-0.m3u8"
        self.deadline = deadline
        self.latencies = []  # seconds, successful responses only
        self.ok = 0
        self.not_ready = 0  # 404 before the stream has its first segment
        self.errors = 0

    def run(self):
        while time.monotonic() < self.deadline:
            start = time.monotonic()
            try:
                with urllib.request.urlopen(self.url, timeout=5) as r:
                    r.read()
                    self.latencies.append(time.monotonic() - start)
                    self.ok += 1
            except urllib.error.HTTPError as e:
                if e.code == 404:
                    self.not_ready += 1
                else:
                    self.errors += 1
                time.sleep(0.05)
            except OSError:
                self.errors += 1
                time.sleep(0.05)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--tpt", default=os.path.join("target", "release", "tpt-kinetix"))
    ap.add_argument("--publishers", type=int, default=4)
    ap.add_argument("--viewers", type=int, default=16)
    ap.add_argument("--seconds", type=int, default=20)
    ap.add_argument("--source", help="existing WebM to publish (default: generate one)")
    ap.add_argument("--out", help="also write the JSON summary here")
    args = ap.parse_args()

    args.tpt = os.path.abspath(args.tpt)
    work = tempfile.mkdtemp(prefix="tpt-live-load-")
    src = args.source or os.path.join(work, "src.webm")
    if not args.source:
        make_source(src, args.seconds + 10)

    port = free_port()
    base = f"http://127.0.0.1:{port}"
    server = subprocess.Popen(
        [args.tpt, "live", "--port", str(port), "--segment-seconds", "2"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    procs = [server]
    try:
        # Wait for the server to listen.
        for _ in range(100):
            try:
                socket.create_connection(("127.0.0.1", port), timeout=0.2).close()
                break
            except OSError:
                time.sleep(0.1)
        else:
            raise SystemExit("the server did not start listening")

        for i in range(args.publishers):
            procs.append(subprocess.Popen(
                ["ffmpeg", "-v", "error", "-re", "-i", src, "-c", "copy", "-f", "webm",
                 "-method", "POST", "-chunked_post", "1", f"{base}/ingest/pub{i}"],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            ))

        # Give the publishers a moment to produce a first segment.
        time.sleep(3)
        baseline_kib = rss_kib(server.pid)  # the server before any publisher
        deadline = time.monotonic() + args.seconds
        viewers = [Viewer(base, f"pub{i % max(1, args.publishers)}", deadline)
                   for i in range(args.viewers)]
        samples = []

        def sample():
            while time.monotonic() < deadline:
                kib = rss_kib(server.pid)
                if kib is not None:
                    samples.append(kib)
                time.sleep(1)

        sampler = threading.Thread(target=sample, daemon=True)
        started = time.monotonic()
        cpu_start = cpu_seconds(server.pid)
        cpu_probe = start_cpu_probe(args.tpt, int(args.seconds) + 1)
        sampler.start()
        for v in viewers:
            v.start()
        for v in viewers:
            v.join()
        # With no viewers the window still runs to its full length.
        time.sleep(max(0.0, deadline - time.monotonic()))
        elapsed = time.monotonic() - started
        cpu_end = cpu_seconds(server.pid)
        sampler.join(timeout=2)
        cores = read_cpu_probe(cpu_probe)
        if cores is None and cpu_start is not None and cpu_end is not None and elapsed:
            cores = (cpu_end - cpu_start) / elapsed
        median_kib = statistics.median(samples) if samples else None

        latencies = sorted(x for v in viewers for x in v.latencies)
        ok = sum(v.ok for v in viewers)
        summary = {
            "machine": platform.platform(),
            "publishers": args.publishers,
            "viewers": args.viewers,
            "seconds": round(elapsed, 1),
            "requests_ok": ok,
            "requests_not_ready": sum(v.not_ready for v in viewers),
            "requests_error": sum(v.errors for v in viewers),
            "requests_per_second": round(ok / elapsed, 1) if elapsed else None,
            "latency_ms": {
                "mean": round(statistics.fmean(latencies) * 1000, 2) if latencies else None,
                "p50": round(percentile(latencies, 50) * 1000, 2) if latencies else None,
                "p95": round(percentile(latencies, 95) * 1000, 2) if latencies else None,
                "p99": round(percentile(latencies, 99) * 1000, 2) if latencies else None,
                "max": round(latencies[-1] * 1000, 2) if latencies else None,
            },
            "server_rss_mib": {
                "min": round(min(samples) / 1024, 1) if samples else None,
                "max": round(max(samples) / 1024, 1) if samples else None,
            },
            "server_cpu_cores": round(cores, 4) if cores is not None else None,
            # Per publisher: the RSS above the idle server, and the cores it used.
            "rss_per_publisher_mib": (
                round((median_kib - baseline_kib) / 1024 / args.publishers, 2)
                if median_kib is not None and baseline_kib is not None and args.publishers
                else None
            ),
            "cores_per_publisher": (
                round(cores / args.publishers, 6)
                if cores is not None and args.publishers
                else None
            ),
        }
        text = json.dumps(summary, indent=2)
        print(text)
        if args.out:
            os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)
            with open(args.out, "w", encoding="utf-8") as f:
                f.write(text + "\n")
        return 0 if summary["requests_error"] == 0 else 1
    finally:
        for p in procs:
            if p.poll() is None:
                p.kill()
                p.wait()


if __name__ == "__main__":
    sys.exit(main())
