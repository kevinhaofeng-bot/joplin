"""Behavioral tests for read-only whole-product memory accounting.

The breaks caught are omitted descendants/detached helpers, charging unrelated
apps, silently accepting missing/ambiguous footprint evidence, PID reuse, and
overwriting earlier evidence. Values are hand-calculated, not mirror sums.
"""
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import unittest


MODULE_PATH = Path(__file__).with_name("measure-product-memory-observe.py")
SPEC = importlib.util.spec_from_file_location("product_memory_observe", MODULE_PATH)
OBSERVER = importlib.util.module_from_spec(SPEC)
if MODULE_PATH.exists():
    SPEC.loader.exec_module(OBSERVER)


class AccountingTests(unittest.TestCase):
    def required(self, name):
        self.assertTrue(hasattr(OBSERVER, name), f"missing memory observer behavior: {name}")
        return getattr(OBSERVER, name)

    def test_counts_nested_children_and_detached_bundle_helper_only_once(self):
        parse = self.required("parse_processes")
        select = self.required("select_processes")
        processes = parse("""10 1 100 /tmp/Joplin Lite.app/Contents/MacOS/joplin-lite
11 10 20 /tmp/extractor
12 11 30 /tmp/decoder
13 1 40 /tmp/Joplin Lite.app/Contents/Helpers/Joplin Lite Picker.app/Contents/MacOS/picker
14 1 500 /tmp/Joplin Lite.app-other/Contents/MacOS/unrelated
15 1 900 /tmp/Old Joplin Lite.app/Contents/MacOS/joplin-lite
20 1 50 /opt/frpc
21 20 60 /tmp/network-child
22 1 800 /opt/frpc
99 11 777 /tmp/observer
100 99 888 /tmp/footprint
""")
        chosen = select(processes, [10, 20], "/tmp/Joplin Lite.app", observer_pid=99)
        self.assertEqual(sorted(chosen), [10, 11, 12, 13, 20, 21])
        self.assertEqual(sum(p["rss_kib"] for p in chosen.values()), 300)

    def test_unrelated_same_bundle_main_is_not_charged_and_ambiguous_helper_is_refused(self):
        parse = self.required("parse_processes")
        select = self.required("select_processes")
        processes = parse("""10 1 100 /tmp/Joplin Lite.app/Contents/MacOS/joplin-lite
16 1 700 /tmp/Joplin Lite.app/Contents/MacOS/joplin-lite
""")
        self.assertEqual(list(select(processes, [10], "/tmp/Joplin Lite.app", 99)), [10])
        processes.update(parse("13 1 40 /tmp/Joplin Lite.app/Contents/Helpers/picker\n"))
        with self.assertRaisesRegex(ValueError, "ambiguous.*helper"):
            select(processes, [10], "/tmp/Joplin Lite.app", 99)

    def test_missing_root_fails_instead_of_reporting_partial_memory(self):
        processes = self.required("parse_processes")("10 1 100 /tmp/app\n")
        with self.assertRaisesRegex(ValueError, "root.*20"):
            self.required("select_processes")(processes, [10, 20], None, observer_pid=99)

    def test_rejects_empty_or_duplicate_process_snapshot(self):
        parse = self.required("parse_processes")
        for text in ("", "10 1 100 /tmp/app\n10 1 100 /tmp/app\n", "10 1 bad /tmp/app\n"):
            with self.subTest(text=text), self.assertRaises(ValueError):
                parse(text)

    def test_footprint_sums_exact_requested_pids_not_other_processes(self):
        data = {
            "unit": "byte", "bytes per unit": 1,
            "processes": [
                {"pid": 10, "footprint": 1000, "auxiliary": {"phys_footprint_peak": 2000}},
                {"pid": 20, "footprint": 3000, "auxiliary": {"phys_footprint_peak": 4000}},
                {"pid": 999, "footprint": 8000, "auxiliary": {"phys_footprint_peak": 9000}},
            ],
        }
        evidence = self.required("parse_footprint")(data, [10, 20])
        self.assertEqual(evidence["total_bytes"], 4000)
        self.assertEqual(evidence["sum_process_lifetime_peaks_bytes"], 6000)
        self.assertEqual(sorted(evidence["processes"]), [10, 20])

    def test_missing_duplicate_wrong_units_invalid_peak_fail_closed(self):
        parse = self.required("parse_footprint")
        good = {"pid": 10, "footprint": 1000, "auxiliary": {"phys_footprint_peak": 2000}}
        cases = [
            {"unit": "byte", "bytes per unit": 1, "processes": []},
            {"unit": "byte", "bytes per unit": 1, "processes": [good, good]},
            {"unit": "KB", "bytes per unit": 1024, "processes": [good]},
            {"unit": "byte", "bytes per unit": 1, "processes": [{**good, "footprint": True}]},
            {"unit": "byte", "bytes per unit": 1, "processes": [{**good, "footprint": -1}]},
            {"unit": "byte", "bytes per unit": 1, "processes": [{**good, "auxiliary": {"phys_footprint_peak": 999}}]},
        ]
        for data in cases:
            with self.subTest(data=data), self.assertRaises(ValueError):
                parse(data, [10])

    def test_changed_executable_or_start_time_invalidates_root_identity(self):
        check = self.required("validate_identity")
        expected = {"executable": "/tmp/app", "start_time": "Mon Oct 5 03:00:00 2026"}
        check(10, {"executable": "/tmp/app"}, expected["start_time"], expected)
        with self.assertRaises(ValueError):
            check(10, {"executable": "/tmp/other"}, expected["start_time"], expected)
        with self.assertRaises(ValueError):
            check(10, {"executable": "/tmp/app"}, "Mon Oct 5 04:00:00 2026", expected)

    def test_a_helper_appearing_during_footprint_invalidates_the_sample(self):
        validate = self.required("validate_membership")
        validate({10: {}, 20: {}}, {10: {}, 20: {}})
        with self.assertRaises(ValueError):
            validate({10: {}, 20: {}}, {10: {}, 20: {}, 30: {}})
        with self.assertRaises(ValueError):
            validate({10: {}, 20: {}}, {10: {}})

    def test_existing_output_is_refused_and_preserved(self):
        create = self.required("create_output")
        with tempfile.TemporaryDirectory() as root:
            existing = Path(root) / "evidence"
            existing.mkdir()
            marker = existing / "previous.json"
            marker.write_text('{"verified":true}\n')
            with self.assertRaises(FileExistsError):
                create(existing)
            self.assertEqual(json.loads(marker.read_text()), {"verified": True})
            created = create(Path(root) / "new")
            self.assertEqual(created.stat().st_mode & 0o777, 0o700)

    def test_range_includes_every_round_and_never_claims_full_acceptance(self):
        summarize = self.required("summarize")
        samples = [
            {"round": 1, "total_rss_kib": 110 * 1024, "total_footprint_bytes": 130 * 1048576},
            {"round": 2, "total_rss_kib": 121 * 1024, "total_footprint_bytes": 140 * 1048576},
            {"round": 3, "total_rss_kib": 115 * 1024, "total_footprint_bytes": 135 * 1048576},
        ]
        result = summarize(samples, rss_limit_mib=120)
        self.assertEqual(result["rss_kib_range"], [112640, 123904])
        self.assertEqual(result["physical_footprint_bytes_range"], [136314880, 146800640])
        self.assertFalse(result["observed_rss_within_limit"])
        self.assertFalse(result["full_product_acceptance"])
        self.assertEqual(result["rounds"], 3)
        with self.assertRaises(ValueError):
            summarize([], rss_limit_mib=120)


@unittest.skipUnless(sys.platform == "darwin", "real ps/footprint integration requires macOS")
class ProcessIntegrationTests(unittest.TestCase):
    def test_cli_reads_real_parent_child_without_terminating_either_or_overwriting_evidence(self):
        # Owned, bounded throwaway workers, not the notes app or a personal
        # library. No ps/footprint mocks: exercise the actual OS boundary.
        worker = subprocess.Popen([
            sys.executable, "-c",
            "import subprocess,sys,time; child=subprocess.Popen([sys.executable,'-c','import time; time.sleep(25)']); "
            "print(child.pid,flush=True); time.sleep(25)",
        ], text=True, stdout=subprocess.PIPE)
        child_pid = None
        try:
            child_pid = int(worker.stdout.readline().strip())
            executable = subprocess.check_output(["/bin/ps", "-p", str(worker.pid), "-o", "comm="], text=True).strip()
            with tempfile.TemporaryDirectory() as root:
                output = Path(root) / "observation"
                command = [sys.executable, str(MODULE_PATH), "--pid", str(worker.pid),
                           "--executable", executable, "--output", str(output),
                           "--scenario", "owned parent/child integration", "--rounds", "1",
                           "--samples", "1", "--interval", "0"]
                completed = subprocess.run(command, text=True, capture_output=True, timeout=20)
                self.assertEqual(completed.returncode, 0, completed.stdout + completed.stderr)
                record = json.loads((output / "round-1-sample-1.json").read_text())
                self.assertEqual(sorted(p["pid"] for p in record["processes"]), sorted([worker.pid, child_pid]))
                summary = output / "summary.json"
                before = summary.read_bytes()
                result = json.loads(before)
                self.assertTrue(result["evidence_complete"])
                self.assertFalse(result["full_product_acceptance"])
                self.assertGreater(record["total_footprint_bytes"], 0)
                os.kill(worker.pid, 0)
                os.kill(child_pid, 0)
                repeat = subprocess.run(command, text=True, capture_output=True, timeout=20)
                self.assertNotEqual(repeat.returncode, 0)
                self.assertEqual(summary.read_bytes(), before)
                os.kill(worker.pid, 0)
                os.kill(child_pid, 0)
        finally:
            if child_pid:
                try:
                    os.kill(child_pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
            worker.terminate()
            worker.wait(timeout=5)
            worker.stdout.close()


if __name__ == "__main__":
    unittest.main()
