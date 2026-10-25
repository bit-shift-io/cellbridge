"""Exercise actual Linux policy lookup in disposable, rootless network namespaces.

Only the HTTPS response is simulated. No packets reach the public internet;
dummy links and routes exist exclusively in the child network namespace.
"""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

WORKER = Path(__file__).resolve().parents[1] / "cellbridge-failover.sh"

KERNEL_SCENARIO = r'''
import os, pathlib, subprocess, time
worker = os.environ["WORKER"]
root = pathlib.Path(os.environ["NS_ROOT"])
def ip(*args, ok=True):
    p = subprocess.run(["ip", "-4", *args], capture_output=True, text=True)
    if ok and p.returncode:
        raise AssertionError(p.stderr)
    return p
for name, address in (("br-lan", "192.168.50.1/24"), ("wan", "192.0.2.2/24"), ("wg0", "10.10.10.1/24")):
    ip("link", "add", name, "type", "dummy")
    ip("addr", "add", address, "dev", name)
    ip("link", "set", name, "up")
ip("route", "add", "default", "via", "192.0.2.1", "dev", "wan", "metric", "2")
# Vendor local-output rules must not steal tunnel probes during recovery.
ip("rule", "add", "priority", "1", "iif", "lo", "lookup", "16800")
ip("route", "add", "default", "dev", "wan", "table", "16800")
if os.environ["NS_SCENARIO"] == "foreign":
    ip("route", "add", "default", "dev", "wan", "table", "51820", "proto", "4")
    for action in ("check", "cleanup"):
        p = subprocess.run(["sh", worker, action], capture_output=True, text=True)
        assert p.returncode != 0, (action, p.stdout, p.stderr)
    assert "dev wan" in ip("route", "show", "table", "51820").stdout
    raise SystemExit(0)

def selected_device():
    return ip("route", "get", "203.0.113.10", "from", "192.168.50.2", "iif", "br-lan").stdout

def wait_path(path):
    deadline = time.monotonic() + 18
    while time.monotonic() < deadline:
        assert daemon.poll() is None, "monitor exited unexpectedly"
        status = root / "run/status"
        if status.exists() and f"path={path}\n" in status.read_text():
            return
        time.sleep(0.05)
    raise AssertionError("monitor failed to select " + path)

def probe_route():
    return ip("route", "get", "1.1.1.1", "from", "10.10.10.1", "oif", "wg0").stdout

daemon = subprocess.Popen(["sh", worker, "run"], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
try:
    wait_path("backup")
    assert "dev wan" in selected_device()
    assert "table 51821" in probe_route() and "dev wg0" in probe_route()
    (root / "healthy").touch()
    wait_path("tunnel")
    assert "dev wg0" in selected_device()
    local = ip("route", "get", "192.168.50.3", "from", "192.168.50.2", "iif", "br-lan").stdout
    assert "dev br-lan" in local
    (root / "healthy").unlink()
    wait_path("backup")
    assert "dev wan" in selected_device()
    assert "table 51821" in probe_route()
    (root / "healthy").touch()
    wait_path("tunnel")
    assert "dev wg0" in selected_device()
finally:
    daemon.terminate()
    _, errors = daemon.communicate(timeout=10)
    assert daemon.returncode == 0, errors

# Cleanup preserves native rules, local destinations and physical defaults.
assert not ip("route", "show", "table", "51820", ok=False).stdout.strip()
assert not ip("route", "show", "table", "51821", ok=False).stdout.strip()
assert "lookup 16800" in ip("rule", "show").stdout
assert "dev wan" in selected_device()

# A missing tunnel route must hit the guard, not vendor/physical fallback.
ip("rule", "add", "priority", "0", "from", "10.10.10.1/32", "lookup", "51821", "protocol", "186")
ip("route", "add", "unreachable", "default", "metric", "32767", "proto", "186", "table", "51821")
guard_lookup = ip("route", "get", "1.1.1.1", "from", "10.10.10.1", "oif", "wg0", ok=False)
# Linux may synthesize an on-link route for an explicitly bound device even
# when FIB lookup fails. It must remain wg0, never escape through the WAN.
assert guard_lookup.returncode != 0 or "dev wg0" in guard_lookup.stdout, guard_lookup.stdout
assert ip("route", "get", "1.1.1.1", "from", "10.10.10.1", ok=False).returncode != 0
'''


@unittest.skipUnless(shutil.which("unshare") and shutil.which("ip"), "requires unshare and iproute2")
class KernelRoutingTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        result = subprocess.run(["unshare", "-Urn", "true"], capture_output=True)
        if result.returncode:
            raise unittest.SkipTest("unprivileged user/network namespaces are unavailable")

    def run_scenario(self, scenario):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "bin").mkdir()
            curl = root / "bin/curl"
            curl.write_text('#!/bin/sh\n[ -f "$NS_ROOT/healthy" ] || exit 28\nprintf 200\n')
            curl.chmod(0o755)
            result = subprocess.run(
                ["unshare", "-Urn", sys.executable, "-c", KERNEL_SCENARIO],
                env={**os.environ, "NS_ROOT": tmp, "NS_SCENARIO": scenario,
                     "WORKER": str(WORKER), "CELLBRIDGE_RUN_DIR": str(root / "run"),
                     "PATH": f"{root / 'bin'}:{os.environ['PATH']}"},
                capture_output=True, text=True, timeout=55,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_real_policy_routing_failover_recovery_and_probe_isolation(self):
        self.run_scenario("failover")

    def test_foreign_routes_are_preserved_during_check_and_cleanup(self):
        self.run_scenario("foreign")


if __name__ == "__main__":
    unittest.main()
