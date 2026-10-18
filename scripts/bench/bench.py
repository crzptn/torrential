#!/usr/bin/env python3
"""Rerunnable performance benchmarks for torrential on macOS.

    python3 -I scripts/bench/bench.py SUITE --bin target/release/torrential --out /tmp/bench [options]

Suites:
  cli         CLI latency with the engine running: --version, status, and a cached search.
  cold        The first command when no engine runs, and where that time goes.
  startup     Process start of torrential --version against floor binaries given by --floor.
  search      Live search with --fresh, spread over time, then the cached repeat.
  cache-idle  Whether a search is still cached after the engine sat idle.
  rest        Engine CPU, wakeups, and memory with nothing to do.
  tui-download  TUI CPU in tmux while the real engine downloads --torrent.
  download    torrential against aria2c on one .torrent, alternating, checked by SHA-256.
  http        The same file over plain HTTP, to show what the link carries.
  summary     Median, p95, and range per case from runs.jsonl.

Every run appends one JSON object to OUT/runs.jsonl with its wall time, whether it did
the work (exit code and output checked), and the 1-minute load average. Each suite uses
its own TORRENTIAL_HOME under OUT with downloads.dir under OUT, deletes what it
downloaded, and stops every engine it started. CPU time and memory come from
proc_pid_rusage, which reads a process's kernel counters without attaching to it.
"""

import argparse
import ctypes
import ctypes.util
import hashlib
import itertools
import json
import math
import os
import re
import shutil
import socket
import statistics
import subprocess
import sys
import threading
import time
from pathlib import Path

QUERY = ["ubuntu", "24.04"]
MIB = 1 << 20

libc = ctypes.CDLL(ctypes.util.find_library("c"), use_errno=True)


class _Timebase(ctypes.Structure):
    _fields_ = [("numer", ctypes.c_uint32), ("denom", ctypes.c_uint32)]


_tb = _Timebase()
libc.mach_timebase_info(ctypes.byref(_tb))
# rusage_info_v4 from <sys/resource.h>, after its 16-byte uuid.
_RUSAGE_FIELDS = (
    "user system pkg_idle_wkups interrupt_wkups pageins wired_size resident_size phys_footprint "
    "proc_start_abstime proc_exit_abstime child_user_time child_system_time child_pkg_idle_wkups "
    "child_interrupt_wkups child_pageins child_elapsed_abstime diskio_bytesread diskio_byteswritten "
    "qos_default qos_maintenance qos_background qos_utility qos_legacy qos_user_initiated "
    "qos_user_interactive billed_system_time serviced_system_time logical_writes "
    "lifetime_max_phys_footprint instructions cycles billed_energy serviced_energy "
    "interval_max_phys_footprint runnable_time"
).split()


class _RusageV4(ctypes.Structure):
    _fields_ = [("uuid", ctypes.c_uint8 * 16)] + [(f, ctypes.c_uint64) for f in _RUSAGE_FIELDS]


def usage(pid):
    """CPU seconds, wakeups, and memory of a live process, or None once it is gone."""
    r = _RusageV4()
    if libc.proc_pid_rusage(pid, 4, ctypes.byref(r)) != 0:
        return None
    return {
        "cpu_s": (r.user + r.system) * _tb.numer / _tb.denom / 1e9,
        "wakeups": r.pkg_idle_wkups + r.interrupt_wkups,
        "rss": r.resident_size,
        "footprint": r.phys_footprint,
        "max_footprint": r.lifetime_max_phys_footprint,
        "disk_written": r.diskio_byteswritten,
    }


class Sampler(threading.Thread):
    """Polls a process's usage, keeping the largest RSS and footprint seen."""

    def __init__(self, pid, every=0.25):
        super().__init__(daemon=True)
        self.pid, self.every = pid, every
        self.peak_rss = self.peak_footprint = 0
        self.last = None
        self.halt = threading.Event()

    def run(self):
        while not self.halt.is_set():
            u = usage(self.pid)
            if u:
                self.last = u
                self.peak_rss = max(self.peak_rss, u["rss"])
                self.peak_footprint = max(self.peak_footprint, u["max_footprint"])
            self.halt.wait(self.every)

    def stop(self):
        self.halt.set()
        self.join()


def load1():
    return round(os.getloadavg()[0], 2)


class Recorder:
    def __init__(self, out, suite):
        self.path = Path(out) / "runs.jsonl"
        self.suite = suite

    def __call__(self, case, **fields):
        row = {"suite": self.suite, "case": case, "at": time.strftime("%H:%M:%S"), "load1": load1(), **fields}
        with self.path.open("a") as f:
            f.write(json.dumps(row) + "\n")
        shown = {k: v for k, v in row.items() if k not in ("series", "sources")}
        print(json.dumps(shown), flush=True)
        return row


class Home:
    """A scratch TORRENTIAL_HOME whose downloads go under it too."""

    def __init__(self, bin, out, name):
        self.bin = bin
        self.root = Path(out) / name
        shutil.rmtree(self.root, ignore_errors=True)
        (self.root / "config").mkdir(parents=True)
        self.downloads = self.root / "dl"
        self.downloads.mkdir()
        (self.root / "config" / "config.yaml").write_text(f"downloads:\n  dir: {self.downloads}\n")
        self.env = dict(os.environ, TORRENTIAL_HOME=str(self.root))

    def run(self, *args, timeout=600):
        """Wall milliseconds, exit code, and stdout."""
        wall_ms, rc, out, _ = self.measure(*args, timeout=timeout)
        return wall_ms, rc, out

    def measure(self, *args, timeout=600):
        """Like run, plus the client's CPU milliseconds and peak RSS in MiB from wait4."""
        started = time.perf_counter()
        child = subprocess.Popen(
            [self.bin, *args], env=self.env, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, stdin=subprocess.DEVNULL
        )
        timer = threading.Timer(timeout, child.kill)
        timer.start()
        out = child.stdout.read()
        _, status, ru = os.wait4(child.pid, 0)
        wall_ms = (time.perf_counter() - started) * 1000
        timer.cancel()
        child.stdout.close()
        child.returncode = os.waitstatus_to_exitcode(status)
        client = {
            "client_cpu_ms": round((ru.ru_utime + ru.ru_stime) * 1000, 2),
            "client_rss_mib": round(ru.ru_maxrss / MIB, 1),
        }
        return wall_ms, child.returncode, out.decode(errors="replace"), client

    def engine_pid(self):
        _, _, out = self.run("--json", "daemon", "status")
        return json.loads(out)["pid"]

    def stop(self):
        pid = self.engine_pid()
        if pid:
            self.run("daemon", "stop")
        deadline = time.time() + 20
        while pid and usage(pid) and time.time() < deadline:
            time.sleep(0.05)
        if pid and usage(pid):
            raise SystemExit(f"engine {pid} under {self.root} did not stop")

    def remove(self):
        self.stop()
        shutil.rmtree(self.root, ignore_errors=True)


def percentile(values, p):
    """Nearest rank: the smallest value with at least p percent of runs at or below it."""
    ordered = sorted(values)
    return ordered[max(0, math.ceil(p / 100 * len(ordered)) - 1)]


def search_report(stdout):
    result = json.loads(stdout)["result"]
    sources = [
        {"source": s["source"], "outcome": s["outcome"], "elapsed_ms": s["elapsed_ms"], "results": s.get("results")}
        for s in result["sources"]
    ]
    return result, sources



def suite_cli(a, record):
    home = Home(a.bin, a.out, "cli")
    try:
        home.run("--json", "status")
        warm_ms, rc, out = home.run("--json", "search", *QUERY)
        warmed_at = time.time()
        _, sources = search_report(out)
        record("warm-search", wall_ms=round(warm_ms, 1), ok=rc == 0, sources=sources)
        pid = home.engine_pid()
        engine_before = usage(pid)
        cases = {
            "version": ["--version"],
            "status-json": ["--json", "status"],
            "status-plain": ["status"],
            "search-cached-json": ["--json", "search", *QUERY],
            "search-cached-plain": ["search", *QUERY],
        }
        names = list(cases)
        calls = 0
        for i in range(a.runs):
            for name in names[i % len(names) :] + names[: i % len(names)]:
                wall_ms, rc, out, client = home.measure(*cases[name])
                ok, note = rc == 0, ""
                if name == "search-cached-json" and ok:
                    _, sources = search_report(out)
                    ok = all(s["outcome"] == "cached" for s in sources)
                    note = ",".join(sorted({s["outcome"] for s in sources}))
                elif name == "search-cached-plain" and ok:
                    ok = "sources" in out and "results" in out
                elif name == "status-json" and ok:
                    ok = json.loads(out)["type"] == "jobs"
                elif name == "version" and ok:
                    ok = out.startswith("torrential ")
                calls += name != "version"
                record(name, run=i, wall_ms=round(wall_ms, 2), ok=ok, note=note, **client)
        engine_after = usage(pid)
        same_engine = home.engine_pid() == pid
        record(
            "engine-per-request",
            ok=same_engine and time.time() - warmed_at < 300,
            requests=calls,
            engine_cpu_ms_per_request=round((engine_after["cpu_s"] - engine_before["cpu_s"]) * 1000 / calls, 3),
            seconds_since_warm=round(time.time() - warmed_at, 1),
        )
    finally:
        home.remove()



def suite_cold(a, record):
    home = Home(a.bin, a.out, "cold")
    try:
        previous = None
        for i in range(a.runs + 1):
            home.stop()
            time.sleep(1)
            case = "cold-status-new-home" if i == 0 else "cold-status"
            wall_ms, rc, out, client = home.measure("--json", "status")
            pid = home.engine_pid()
            ok = rc == 0 and json.loads(out)["type"] == "jobs" and pid is not None and pid != previous
            record(case, run=i, wall_ms=round(wall_ms, 1), ok=ok, engine_pid=pid, **client)
            previous = pid
        for i in range(a.runs):
            home.stop()
            time.sleep(1)
            record("cold-parts", run=i, **cold_parts(home))
    finally:
        home.remove()


def cold_parts(home):
    """Times the launcher, the engine's socket, and the first answer separately."""
    sock = home.root / "run" / "engine.sock"
    started = time.perf_counter()
    launcher = subprocess.run(
        [home.bin, "daemon", "launch"], env=home.env, capture_output=True, text=True, start_new_session=True
    )
    launched = time.perf_counter()
    pid = int(launcher.stdout.strip())
    while True:
        try:
            with socket.socket(socket.AF_UNIX) as s:
                s.connect(str(sock))
            break
        except OSError:
            if time.perf_counter() - started > 10:
                return {"ok": False}
            time.sleep(0.002)
    listening = time.perf_counter()
    wall_ms, rc, _ = home.run("--json", "status")
    answered = time.perf_counter()
    engine = usage(pid)
    return {
        "ok": rc == 0,
        "launcher_ms": round((launched - started) * 1000, 1),
        "engine_listen_ms": round((listening - launched) * 1000, 1),
        "first_status_ms": round((answered - listening) * 1000, 1),
        "total_ms": round((answered - started) * 1000, 1),
        "engine_cpu_ms_at_ready": round(engine["cpu_s"] * 1000, 1) if engine else None,
    }



def launch(cmd):
    started = time.perf_counter()
    child = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, stdin=subprocess.DEVNULL)
    _, status, ru = os.wait4(child.pid, 0)
    return (time.perf_counter() - started) * 1000, os.waitstatus_to_exitcode(status), ru


def suite_startup(a, record):
    commands = {"torrential --version": [a.bin, "--version"]} | {Path(f).name: [f] for f in a.floor}
    for i in range(a.runs):
        names = list(commands)
        for name in names[i % len(names) :] + names[: i % len(names)]:
            wall_ms, rc, ru = launch(commands[name])
            record(
                name,
                run=i,
                ok=rc == 0,
                wall_ms=round(wall_ms, 2),
                client_cpu_ms=round((ru.ru_utime + ru.ru_stime) * 1000, 2),
                client_rss_mib=round(ru.ru_maxrss / MIB, 1),
                page_faults=ru.ru_minflt + ru.ru_majflt,
            )



def suite_search(a, record):
    home = Home(a.bin, a.out, "search")
    try:
        for i in range(a.start, a.start + a.runs):
            began = time.time()
            ensure_ms, _, _ = home.run("--json", "status")
            wall_ms, rc, out = home.run("--json", "search", *QUERY, "--fresh")
            result, sources = search_report(out) if rc == 0 else ({}, [])
            slowest = max(sources, key=lambda s: s["elapsed_ms"]) if sources else {}
            failed = [s["source"] for s in sources if s["outcome"] not in ("ok", "cached")]
            record(
                "search-live",
                run=i,
                store="cold" if i == a.start else "warm",
                wall_ms=round(wall_ms, 1),
                engine_elapsed_ms=result.get("elapsed_ms"),
                ensure_engine_ms=round(ensure_ms, 1),
                ok=rc == 0 and len(result.get("releases", [])) > 0,
                failed_sources=failed,
                slowest=slowest.get("source"),
                slowest_ms=slowest.get("elapsed_ms"),
                sources=sources,
            )
            hit_ms, rc, out = home.run("--json", "search", *QUERY)
            _, hit_sources = search_report(out) if rc == 0 else ({}, [])
            cached = [s["source"] for s in hit_sources if s["outcome"] == "cached"]
            record(
                "search-cache-hit",
                run=i,
                wall_ms=round(hit_ms, 2),
                # A source that failed just now may still answer from an earlier search's cache entry.
                ok=rc == 0 and set(s["source"] for s in hit_sources) - set(cached) <= set(failed),
                cached_sources=len(cached),
                refetched=[s["source"] for s in hit_sources if s["outcome"] != "cached"],
            )
            if i + 1 < a.start + a.runs:
                time.sleep(max(0, a.gap - (time.time() - began)))
    finally:
        home.remove()


def suite_cache_idle(a, record):
    home = Home(a.bin, a.out, "cache-idle")
    try:
        _, rc, out = home.run("--json", "search", *QUERY)
        first_pid = home.engine_pid()
        time.sleep(a.idle)
        pid_after_idle = home.engine_pid()
        wall_ms, rc, out = home.run("--json", "search", *QUERY)
        _, sources = search_report(out)
        record(
            "search-after-idle",
            idle_s=a.idle,
            ok=rc == 0,
            engine_survived_idle=pid_after_idle == first_pid,
            wall_ms=round(wall_ms, 1),
            outcomes=sorted({s["outcome"] for s in sources}),
        )
    finally:
        home.remove()



def suite_rest(a, record):
    for i in range(a.runs):
        home = Home(a.bin, a.out, f"rest{i}")
        try:
            home.run("--json", "status")
            pid = home.engine_pid()
            time.sleep(a.settle)
            sampler = Sampler(pid, every=1.0)
            before = usage(pid)
            sampler.start()
            time.sleep(a.seconds)
            after = usage(pid)
            sampler.stop()
            cpu = after["cpu_s"] - before["cpu_s"]
            record(
                "engine-idle",
                run=i,
                ok=home.engine_pid() == pid,
                seconds=a.seconds,
                cpu_ms=round(cpu * 1000, 2),
                cpu_pct=round(cpu * 100 / a.seconds, 3),
                wakeups_per_s=round((after["wakeups"] - before["wakeups"]) / a.seconds, 1),
                rss_mib=round(after["rss"] / MIB, 1),
                peak_rss_mib=round(sampler.peak_rss / MIB, 1),
                footprint_mib=round(after["footprint"] / MIB, 1),
            )
        finally:
            home.remove()


def suite_tui_download(a, record):
    """The TUI in a 100x30 tmux pane while the real engine downloads --torrent."""
    for i in range(a.runs):
        home = Home(a.bin, a.out, f"tui-dl{i}")
        session = f"bench-tui-{os.getpid()}-{i}"
        try:
            home.run("--json", "get", a.torrent, "--dir", str(home.downloads))
            engine = home.engine_pid()
            env = f"TORRENTIAL_HOME={home.root} TERM=xterm-256color COLORTERM=truecolor"
            tui_cmd = f"exec env {env} {a.bin} tui"
            subprocess.run(["tmux", "new-session", "-d", "-s", session, "-x", "100", "-y", "30", tui_cmd], check=True)
            panes = ["tmux", "list-panes", "-t", session, "-F", "#{pane_pid}"]
            tui = int(subprocess.run(panes, capture_output=True, text=True).stdout)
            time.sleep(a.settle)
            t0, e0 = usage(tui), usage(engine)
            time.sleep(a.seconds)
            t1, e1 = usage(tui), usage(engine)
            pane = subprocess.run(["tmux", "capture-pane", "-p", "-t", session], capture_output=True, text=True).stdout
            header = pane.splitlines()[0] if pane else ""
            record(
                "tui-real-download",
                run=i,
                ok=t1 is not None and "/s" in header,
                seconds=a.seconds,
                tui_cpu_ms=round((t1["cpu_s"] - t0["cpu_s"]) * 1000, 1),
                tui_cpu_pct=round((t1["cpu_s"] - t0["cpu_s"]) * 100 / a.seconds, 3),
                tui_wakeups_per_s=round((t1["wakeups"] - t0["wakeups"]) / a.seconds, 1),
                tui_rss_mib=round(t1["rss"] / MIB, 1),
                engine_cpu_pct=round((e1["cpu_s"] - e0["cpu_s"]) * 100 / a.seconds, 2),
                header=header.strip(),
            )
        finally:
            subprocess.run(["tmux", "kill-session", "-t", session], capture_output=True)
            home.remove()



def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        while chunk := f.read(8 * MIB):
            h.update(chunk)
    return h.hexdigest()


def torrential_once(a, i, record):
    home = Home(a.bin, a.out, f"dl-t{i}")
    try:
        ensure_ms, _, _ = home.run("--json", "status")
        pid = home.engine_pid()
        sampler = Sampler(pid)
        before = usage(pid)
        sampler.start()
        series = []
        started = time.perf_counter()
        _, _, out = home.run("--json", "get", a.torrent, "--dir", str(home.downloads))
        job = json.loads(out)["reply"]["job"]
        short = job["id"][:6]
        waiter = subprocess.Popen(
            [a.bin, "--json", "wait", short, "--timeout", "60m"], env=home.env, stdout=subprocess.PIPE, text=True
        )
        while waiter.poll() is None:
            _, _, out = home.run("--json", "status", short)
            p = json.loads(out)["jobs"][0]["progress"]
            elapsed = round(time.perf_counter() - started, 1)
            series.append([elapsed, p["verified"], p["down_rate"], p["peers"], p["seeds"]])
            time.sleep(a.poll)
        finished = time.perf_counter()
        wait_out = waiter.stdout.read()
        after = usage(pid)
        sampler.stop()
        outcome = json.loads(wait_out.strip().splitlines()[-1]).get("outcome") if wait_out.strip() else None
        path = home.downloads / job["name"]
        digest = sha256(path) if path.exists() else None
        wall = finished - started
        record(
            "download",
            side="torrential",
            run=i,
            ok=outcome == "done" and digest == a.sha256,
            outcome=outcome,
            sha256_ok=digest == a.sha256,
            wall_s=round(wall, 2),
            mib_per_s=round(path.stat().st_size / MIB / wall, 2) if path.exists() else None,
            cpu_s=round(after["cpu_s"] - before["cpu_s"], 2),
            peak_rss_mib=round(sampler.peak_rss / MIB, 1),
            peak_footprint_mib=round(after["max_footprint"] / MIB, 1),
            disk_written_mib=round((after["disk_written"] - before["disk_written"]) / MIB, 1),
            engine_start_ms=round(ensure_ms, 1),
            first_byte_s=next((s[0] for s in series if s[1] > 0), None),
            series=series,
        )
        home.run("rm", short)
    finally:
        home.remove()


AQ = re.compile(r"\[#\w+ ([\d.]+)(\w+)/([\d.]+)\w+\(\d+%\) CN:(\d+)(?: SD:(\d+))? DL:([\d.]+)(\w+)")
UNITS = {"B": 1, "KiB": 1 << 10, "MiB": MIB, "GiB": 1 << 30}


def aria2_once(a, i, record, args=None, side="aria2c"):
    args = a.aria2_arg if args is None else args
    root = Path(a.out) / f"dl-a{i}"
    shutil.rmtree(root, ignore_errors=True)
    root.mkdir(parents=True)
    cmd = [
        a.aria2c,
        "--no-conf",
        "--seed-time=0",
        f"--dir={root}",
        f"--dht-file-path={root}/dht.dat",
        f"--dht-file-path6={root}/dht6.dat",
        f"--summary-interval={int(a.poll)}",
        "--console-log-level=warn",
        "--enable-color=false",
        "--show-console-readout=false",
        *args,
        a.torrent,
    ]
    try:
        started = time.perf_counter()
        child = subprocess.Popen(cmd, env=dict(os.environ, HOME=str(root)), stdout=subprocess.PIPE, text=True)
        sampler = Sampler(child.pid)
        sampler.start()
        series = []
        for line in child.stdout:
            m = AQ.search(line)
            if m:
                done, rate = float(m[1]) * UNITS.get(m[2], 1), float(m[6]) * UNITS.get(m[7], 1)
                elapsed = round(time.perf_counter() - started, 1)
                series.append([elapsed, int(done), int(rate), int(m[4]), int(m[5] or 0)])
        _, status, ru = os.wait4(child.pid, 0)
        finished = time.perf_counter()
        sampler.stop()
        path = root / Path(a.torrent).name.removesuffix(".torrent")
        digest = sha256(path) if path.exists() else None
        wall = finished - started
        last = sampler.last or {}
        record(
            "download",
            side=side,
            run=i,
            ok=os.waitstatus_to_exitcode(status) == 0 and digest == a.sha256,
            exit=os.waitstatus_to_exitcode(status),
            sha256_ok=digest == a.sha256,
            wall_s=round(wall, 2),
            mib_per_s=round(path.stat().st_size / MIB / wall, 2) if path.exists() else None,
            cpu_s=round(ru.ru_utime + ru.ru_stime, 2),
            peak_rss_mib=round(ru.ru_maxrss / MIB, 1),
            peak_footprint_mib=round(last.get("max_footprint", 0) / MIB, 1),
            disk_written_mib=round(last.get("disk_written", 0) / MIB, 1),
            args=args,
            first_byte_s=next((s[0] for s in series if s[1] > 0), None),
            series=series,
        )
    finally:
        shutil.rmtree(root, ignore_errors=True)


def suite_download(a, record):
    sides = {
        "t": torrential_once,
        "a": aria2_once,
        "A": lambda a, i, record: aria2_once(a, i, record, a.aria2_tuned_arg, "aria2c-tuned"),
        "h": http_once,
    }
    for i in range(a.start, a.start + a.runs):
        # Each round starts one side later, so no side always runs first.
        k = i % len(a.order) if a.rotate else 0
        for side in a.order[k:] + a.order[:k]:
            sides[side](a, i, record)


def suite_http(a, record):
    for i in range(a.runs):
        http_once(a, i, record)


def http_once(a, i, record):
    root = Path(a.out) / "http"
    shutil.rmtree(root, ignore_errors=True)
    root.mkdir(parents=True)
    path = root / "file"
    try:
        started = time.perf_counter()
        child = subprocess.Popen(
            ["curl", "-sS", "-L", "-o", str(path), "-w", "%{http_code} %{remote_ip}", a.url],
            stdout=subprocess.PIPE,
            text=True,
        )
        out = child.stdout.read()
        _, status, ru = os.wait4(child.pid, 0)
        wall = time.perf_counter() - started
        size = path.stat().st_size if path.exists() else 0
        digest = sha256(path) if a.sha256 and path.exists() else None
        record(
            "http",
            side="http",
            run=i,
            url=a.url,
            ok=os.waitstatus_to_exitcode(status) == 0
            and out.startswith("200")
            and (digest == a.sha256 if a.sha256 else size > 0),
            code_ip=out.strip(),
            sha256_ok=digest == a.sha256 if a.sha256 else None,
            wall_s=round(wall, 2),
            mib=round(size / MIB, 1),
            mib_per_s=round(size / MIB / wall, 2),
            cpu_s=round(ru.ru_utime + ru.ru_stime, 2),
            peak_rss_mib=round(ru.ru_maxrss / MIB, 1),
        )
    finally:
        shutil.rmtree(root, ignore_errors=True)


METRICS = ("wall_ms", "wall_s", "mib_per_s", "cpu_s", "cpu_ms", "cpu_pct", "client_cpu_ms", "peak_rss_mib",
           "peak_footprint_mib", "rss_mib", "footprint_mib", "wakeups_per_s", "launcher_ms", "engine_listen_ms",
           "first_status_ms", "total_ms", "engine_elapsed_ms", "slowest_ms", "first_byte_s", "page_faults")


def suite_summary(a, _record):
    rows = [json.loads(line) for line in (Path(a.out) / "runs.jsonl").read_text().splitlines() if line.strip()]
    groups = {}
    for row in rows:
        key = (row["suite"], row["case"], row.get("side", ""), row.get("store", ""))
        groups.setdefault(key, []).append(row)
    print("| suite | case | side | n | failed | metric | median | p95 | min | max | load1 |")
    print("|---|---|---|---|---|---|---|---|---|---|---|")
    for (suite, case, side, store), group in groups.items():
        good = [r for r in group if r.get("ok")]
        loads = [r["load1"] for r in group]
        label = f"{case} ({store})" if store else case
        for metric in METRICS:
            values = [r[metric] for r in good if isinstance(r.get(metric), (int, float))]
            if not values:
                continue
            p95 = percentile(values, 95) if len(values) >= 20 else ""
            print(
                f"| {suite} | {label} | {side} | {len(values)} | {len(group) - len(good)} | {metric} | "
                f"{statistics.median(values):g} | {p95} | {min(values):g} | {max(values):g} | "
                f"{min(loads):g}-{max(loads):g} |"
            )
    download_summary(rows)


def ramp(row):
    """Seconds to the first 100 MiB, and MiB/s between 20% and 80% of the file."""
    series = row.get("series") or []
    size = row["mib_per_s"] * row["wall_s"] * MIB
    at = lambda done: next((t for t, got, *_ in series if got >= done), None)
    t20, t80 = at(0.2 * size), at(0.8 * size)
    steady = 0.6 * size / MIB / (t80 - t20) if t20 is not None and t80 is not None and t80 > t20 else None
    return at(100 * MIB), steady


def rank_sum_p(xs, ys):
    """Exact two-sided Mann-Whitney U p-value, by enumerating every split of the pooled runs."""
    pooled = xs + ys
    observed = sum((a > b) + 0.5 * (a == b) for a in xs for b in ys)
    mean = len(xs) * len(ys) / 2
    extreme = total = 0
    for idx in itertools.combinations(range(len(pooled)), len(xs)):
        idx = set(idx)
        chosen = [pooled[k] for k in idx]
        rest = [pooled[k] for k in range(len(pooled)) if k not in idx]
        value = sum((a > b) + 0.5 * (a == b) for a in chosen for b in rest)
        extreme += abs(value - mean) >= abs(observed - mean) - 1e-9
        total += 1
    return extreme / total


def download_summary(rows):
    runs = [r for r in rows if r["case"] in ("download", "http") and r.get("ok")]
    if not runs:
        return
    print()
    print(
        "| side | n | wall_s median (range) | MiB/s median | to 100 MiB s | steady 20-80% MiB/s | "
        "cpu_s median (range) | peak RSS MiB | peak footprint MiB |"
    )
    print("|---|---|---|---|---|---|---|---|---|")
    sides = {}
    for r in runs:
        sides.setdefault(r.get("side", "http"), []).append(r)
    med = lambda vs: statistics.median(vs) if vs else None
    for side, group in sides.items():
        walls = [r["wall_s"] for r in group]
        cpus = [r["cpu_s"] for r in group]
        ramps = [ramp(r) for r in group]
        first = [f for f, _ in ramps if f is not None]
        steady = [st for _, st in ramps if st is not None]
        fmt = lambda v: "" if v is None else f"{v:.1f}"
        print(
            f"| {side} | {len(group)} | {med(walls):.1f} ({min(walls):.1f}-{max(walls):.1f}) | "
            f"{med([r['mib_per_s'] for r in group]):.2f} | {fmt(med(first))} | {fmt(med(steady))} | "
            f"{med(cpus):.2f} ({min(cpus):.2f}-{max(cpus):.2f}) | {med([r['peak_rss_mib'] for r in group]):.1f} | "
            f"{fmt(med([r['peak_footprint_mib'] for r in group if 'peak_footprint_mib' in r]))} |"
        )
    base = [r["wall_s"] for r in sides.get("torrential", [])]
    for side, group in sides.items():
        if side != "torrential" and base:
            other = [r["wall_s"] for r in group]
            p = rank_sum_p(base, other)
            print(f"wall_s torrential vs {side}: exact rank-sum p = {p:.3f} (n = {len(base)} and {len(other)})")
    cpu_base = [r["cpu_s"] for r in sides.get("torrential", [])]
    for side, group in sides.items():
        if side not in ("torrential", "http") and cpu_base:
            other = [r["cpu_s"] for r in group]
            print(f"cpu_s torrential vs {side}: exact rank-sum p = {rank_sum_p(cpu_base, other):.3f}")


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument(
        "suite",
        choices=[
            "cli", "cold", "startup", "search", "cache-idle", "rest", "tui-download", "download", "http", "summary",
        ],
    )
    p.add_argument("--bin", default="target/release/torrential")
    p.add_argument("--out", required=True)
    p.add_argument("--runs", type=int, default=5)
    p.add_argument("--start", type=int, default=0, help="download, search: index of the first run, to extend a set")
    p.add_argument("--gap", type=float, default=90, help="search: seconds between live searches")
    p.add_argument("--idle", type=float, default=150, help="cache-idle: seconds to leave the engine alone")
    p.add_argument("--settle", type=float, default=5, help="rest, tui-download: seconds after start before measuring")
    p.add_argument("--seconds", type=float, default=60, help="rest, tui-download: measurement window")
    p.add_argument("--torrent", help="download: the .torrent both clients fetch")
    p.add_argument("--sha256", help="download, http: expected SHA-256 of the file")
    p.add_argument("--aria2c", default="aria2c")
    p.add_argument("--aria2-arg", action="append", default=[], help="download: extra aria2c flag, repeatable")
    p.add_argument(
        "--order",
        default="ta",
        help="download: t torrential, a aria2c, A aria2c with --aria2-tuned-arg, h HTTP from --url",
    )
    p.add_argument("--rotate", action="store_true", help="download: start each round one side later")
    p.add_argument("--aria2-tuned-arg", action="append", default=[], help="download: aria2c flag for side A")
    p.add_argument("--poll", type=float, default=2, help="download: seconds between progress samples")
    p.add_argument("--url", help="http: the file to fetch")
    p.add_argument("--floor", action="append", default=[], help="startup: a binary to launch for comparison")
    a = p.parse_args()
    a.bin = str(Path(a.bin).resolve())
    if a.torrent:
        a.torrent = str(Path(a.torrent).resolve())
    if not a.out.startswith("/tmp/"):
        sys.exit("--out must be under /tmp, so no run touches a real home")
    Path(a.out).mkdir(parents=True, exist_ok=True)
    suite = a.suite.replace("-", "_")
    globals()[f"suite_{suite}"](a, Recorder(a.out, a.suite))


if __name__ == "__main__":
    main()
