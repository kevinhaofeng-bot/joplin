#!/usr/bin/env python3
"""Read-only macOS memory observation of an already-running notes product.

Never launches, terminates, reparents, or changes the app/profile. Explicit
roots plus descendants and detached helpers from the exact app bundle are
counted. The observer's own subtree is excluded. Measurements are observations,
not final product acceptance or cold-start/typical-fixture evidence.
"""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time


def parse_processes(text):
    processes = {}
    for line in text.splitlines():
        if not line.strip():
            continue
        parts = line.strip().split(None, 3)
        if len(parts) != 4:
            raise ValueError("malformed process snapshot")
        try:
            pid, ppid, rss = map(int, parts[:3])
        except ValueError as error:
            raise ValueError("malformed process numbers") from error
        if pid <= 0 or ppid < 0 or rss < 0 or pid in processes:
            raise ValueError("invalid or duplicate process snapshot")
        processes[pid] = {"pid": pid, "ppid": ppid, "rss_kib": rss, "executable": parts[3]}
    if not processes:
        raise ValueError("empty process snapshot")
    return processes


def descendants(processes, roots):
    chosen = set(roots)
    while True:
        expanded = chosen | {pid for pid, p in processes.items() if p["ppid"] in chosen}
        if expanded == chosen:
            return chosen
        chosen = expanded


def select_processes(processes, roots, bundle, observer_pid):
    for pid in roots:
        if pid not in processes:
            raise ValueError(f"missing root process {pid}")
    owned = descendants(processes, roots)
    seeds = set(roots)
    if bundle:
        # Only helper service directories, never other main instances of the
        # same bundle (which can belong to a different profile). If another
        # main instance is live, detached helper ownership cannot be proven.
        prefixes = tuple(str(bundle).rstrip("/") + suffix for suffix in
                         ("/Contents/Helpers/", "/Contents/XPCServices/"))
        detached = {pid for pid, p in processes.items()
                    if pid not in owned and p["executable"].startswith(prefixes)}
        primary_executable = processes[roots[0]]["executable"]
        if detached and any(pid not in owned and p["executable"] == primary_executable
                            for pid, p in processes.items()):
            raise ValueError("ambiguous detached helper owner: another main instance is live")
        seeds.update(detached)
    selected = descendants(processes, seeds) - descendants(processes, [observer_pid])
    if not set(roots).issubset(selected):
        raise ValueError("observer subtree contains a requested root")
    return {pid: processes[pid] for pid in sorted(selected) if pid in processes}


def parse_footprint(data, pids):
    if data.get("unit") != "byte" or type(data.get("bytes per unit")) is not int or data["bytes per unit"] != 1:
        raise ValueError("footprint must report byte units")
    records = data.get("processes")
    if not isinstance(records, list):
        raise ValueError("missing footprint processes")
    selected = {}
    for pid in pids:
        matches = [p for p in records if isinstance(p, dict) and type(p.get("pid")) is int and p["pid"] == pid]
        if len(matches) != 1:
            raise ValueError(f"missing or duplicate footprint for {pid}")
        record = matches[0]
        current = record.get("footprint")
        auxiliary = record.get("auxiliary")
        peak = auxiliary.get("phys_footprint_peak") if isinstance(auxiliary, dict) else None
        if type(current) is not int or current < 0 or type(peak) is not int or peak < current:
            raise ValueError(f"invalid footprint or lifetime peak for {pid}")
        selected[pid] = {"footprint_bytes": current, "lifetime_peak_bytes": peak}
    return {
        "processes": selected,
        "total_bytes": sum(p["footprint_bytes"] for p in selected.values()),
        # Different processes may have peaked at different times. This is NOT
        # a simultaneous whole-product peak.
        "sum_process_lifetime_peaks_bytes": sum(p["lifetime_peak_bytes"] for p in selected.values()),
    }


def validate_identity(pid, process, start_time, expected):
    if process is None or process["executable"] != expected["executable"] or start_time != expected["start_time"]:
        raise ValueError(f"process identity changed or disappeared: {pid}")


def validate_membership(before, after):
    if set(before) != set(after):
        raise ValueError("product process membership changed during footprint sampling")


def create_output(path):
    path = Path(path)
    if not path.is_absolute():
        raise ValueError("output must be absolute")
    path.mkdir(mode=0o700)  # No exist_ok: prior evidence is never overwritten.
    return path


def summarize(samples, rss_limit_mib):
    if not samples:
        raise ValueError("no memory samples")
    rss = [s["total_rss_kib"] for s in samples]
    physical = [s["total_footprint_bytes"] for s in samples]
    return {
        "rounds": len({s["round"] for s in samples}),
        "sample_count": len(samples),
        "rss_kib_range": [min(rss), max(rss)],
        "physical_footprint_bytes_range": [min(physical), max(physical)],
        "observed_rss_limit_mib": rss_limit_mib,
        "observed_rss_within_limit": max(rss) <= rss_limit_mib * 1024,
        "full_product_acceptance": False,
        "scope": "Repeated observations of an existing session, not independent launches, cold-start, latency, or a verified typical fixture.",
    }


def capture(argv):
    result = subprocess.run(argv, check=True, text=True, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, timeout=20)
    return result.stdout.strip()


def process_snapshot():
    return parse_processes(capture(["/bin/ps", "-axo", "pid=,ppid=,rss=,comm="]))


def started(pid):
    value = capture(["/bin/ps", "-p", str(pid), "-o", "lstart="])
    if not value:
        raise ValueError(f"missing process start time: {pid}")
    return value


def digest(path):
    sha = hashlib.sha256()
    with Path(path).open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            sha.update(block)
    return sha.hexdigest()


def write_json(path, value):
    with Path(path).open("x", encoding="utf-8") as handle:
        json.dump(value, handle, sort_keys=True, indent=2)
        handle.write("\n")


def observe(args):
    if sys.platform != "darwin":
        raise ValueError("actual observation requires macOS ps and footprint")
    roots = {args.pid: args.executable}
    for pid_text, executable in args.aux:
        pid = int(pid_text)
        if pid <= 0 or pid in roots:
            raise ValueError("auxiliary PIDs must be positive and unique")
        roots[pid] = executable
    if args.pid <= 0 or args.rounds <= 0 or args.samples <= 0 or not 0 <= args.interval <= 60:
        raise ValueError("invalid PID, rounds, samples or interval")
    first = process_snapshot()
    identities = {}
    for pid, executable in roots.items():
        if not Path(executable).is_absolute() or not Path(executable).is_file():
            raise ValueError("root executable must be an existing absolute file")
        identity = {"executable": executable, "start_time": started(pid)}
        validate_identity(pid, first.get(pid), identity["start_time"], identity)
        identity["sha256"] = digest(executable)
        identities[pid] = identity
    bundle = next((str(p) for p in Path(args.executable).parents if p.name.endswith(".app")), None)
    out = create_output(args.output)
    write_json(out / "identity.json", {
        "roots": identities, "bundle_family": bundle,
        "created_at": datetime.now(timezone.utc).isoformat(), "scenario": args.scenario,
        "machine": capture(["/usr/sbin/sysctl", "-n", "machdep.cpu.brand_string"]),
        "memory_bytes": capture(["/usr/sbin/sysctl", "-n", "hw.memsize"]),
        "macos": capture(["/usr/bin/sw_vers", "-productVersion"]),
    })
    samples = []
    try:
        for round_number in range(1, args.rounds + 1):
            for sample_number in range(1, args.samples + 1):
                processes = process_snapshot()
                selected = select_processes(processes, list(roots), bundle, os.getpid())
                sample_identity = {pid: {"executable": p["executable"], "start_time": started(pid)}
                                   for pid, p in selected.items()}
                for pid, expected in identities.items():
                    validate_identity(pid, processes.get(pid), sample_identity[pid]["start_time"], expected)
                prefix = f"round-{round_number}-sample-{sample_number}"
                raw = out / f"{prefix}-footprint.json"
                with (out / f"{prefix}-footprint.log").open("x") as log:
                    subprocess.run(["/usr/bin/footprint", "-j", str(raw), *map(str, selected)],
                                   stdout=log, stderr=subprocess.STDOUT, check=True, timeout=20)
                with raw.open(encoding="utf-8") as handle:
                    physical = parse_footprint(json.load(handle), list(selected))
                after = process_snapshot()
                validate_membership(selected, select_processes(after, list(roots), bundle, os.getpid()))
                for pid, expected in sample_identity.items():
                    validate_identity(pid, after.get(pid), started(pid), expected)
                sample = {
                    "round": round_number, "sample": sample_number,
                    "timestamp_utc": datetime.now(timezone.utc).isoformat(),
                    "processes": list(selected.values()),
                    "process_start_times": sample_identity,
                    "footprint": physical,
                    "total_rss_kib": sum(p["rss_kib"] for p in selected.values()),
                    "total_footprint_bytes": physical["total_bytes"],
                }
                samples.append(sample)
                write_json(out / f"{prefix}.json", sample)
                print(f"round={round_number} sample={sample_number} processes={len(selected)} "
                      f"rss_mib={sample['total_rss_kib']/1024:.3f} "
                      f"physical_mib={sample['total_footprint_bytes']/1048576:.3f}", flush=True)
                if round_number != args.rounds or sample_number != args.samples:
                    time.sleep(args.interval)
        final = summarize(samples, args.rss_limit_mib)
        final["evidence_complete"] = True
        final["scenario"] = args.scenario
        write_json(out / "summary.json", final)
    except Exception as error:
        write_json(out / "incomplete.json", {"evidence_complete": False, "error": str(error),
                                            "retained_samples": len(samples), "full_product_acceptance": False})
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pid", type=int, required=True)
    parser.add_argument("--executable", required=True)
    parser.add_argument("--aux", nargs=2, action="append", default=[], metavar=("PID", "EXECUTABLE"))
    parser.add_argument("--output", required=True)
    parser.add_argument("--scenario", required=True)
    parser.add_argument("--rounds", type=int, default=3)
    parser.add_argument("--samples", type=int, default=6)
    parser.add_argument("--interval", type=float, default=5)
    parser.add_argument("--rss-limit-mib", type=float, default=120)
    args = parser.parse_args()
    try:
        observe(args)
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        print(f"memory evidence incomplete: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
