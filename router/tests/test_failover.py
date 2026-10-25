"""Run the real shell monitor with simulated kernel/network command boundaries.

No root, internet access, router connection or host route changes are required.
"""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


WORKER = Path(__file__).resolve().parents[1] / "cellbridge-failover.sh"

FAKE_COMMAND = r'''#!/usr/bin/env python3
import json, os, pathlib, signal, sys
p = pathlib.Path(os.environ["SIM_STATE"])
s = json.loads(p.read_text())
cmd = pathlib.Path(sys.argv[0]).name
a = sys.argv[1:]
rc, output = 0, ""
if cmd == "ip":
    while a and a[0] in ("-4", "-N", "-details"):
        a = a[1:]
    s["commands"].append(a)
    if a[:2] == ["rule", "show"]:
        lines = []
        for rule in s["rules"]:
            words = rule.split()
            pref = words[1]
            rest = " ".join(words[2:]).replace("protocol ", "proto ").replace("lookup main", "lookup 254").replace("10.10.10.1/32", "10.10.10.1")
            if not rest.startswith("from "):
                rest = "from all " + rest
            lines.append(pref + ": " + rest)
        output = "\n".join(lines)
    elif a[:2] in (["rule", "add"], ["rule", "del"]):
        rule = " ".join(a[2:])
        if a[1] == "add":
            if rule not in s["rules"]:
                s["rules"].append(rule)
        elif rule in s["rules"]:
            s["rules"].remove(rule)
        else:
            rc = 2
    elif a[:1] == ["route"] and "table" in a:
        t = a[a.index("table") + 1]
        route = " ".join(a[2:a.index("table")])
        if a[1] == "flush":
            s["tables"][t] = []
        elif a[1] == "show":
            if t == "all":
                output = "\n".join(r + " table " + name for name, routes in s["tables"].items() for r in routes)
            else:
                output = "\n".join(s["tables"].get(t, []))
        elif a[1] == "replace":
            if t == "51820" and s.get("route_failure"):
                rc = 2
            else:
                # Distinct metrics may coexist (including unreachable guard).
                routes = s["tables"].setdefault(t, [])
                metric = route.split("metric ")[-1].split()[0]
                routes[:] = [r for r in routes if r.split("metric ")[-1].split()[0] != metric]
                routes.append(route)
        else:
            raise RuntimeError(a)
    else:
        raise RuntimeError(a)
elif cmd == "curl":
    s["probes"].append(a)
    mode = s["scenario"][s["cycle"]]
    if mode == "secondary":
        good = "https://1.0.0.1/cdn-cgi/trace" in a
    else:
        good = mode is True
    # A redirected captive-portal page is not internet success.
    output = "302" if mode == "portal" else "200" if good else "000"
    rc = 0 if good or mode == "portal" else 28
elif cmd == "sleep":
    s["samples"].append(json.loads(json.dumps(s["tables"])))
    s["cycle"] += 1
    if s["cycle"] >= len(s["scenario"]):
        os.kill(os.getppid(), signal.SIGTERM)
elif cmd == "logger":
    s["logs"].append(" ".join(a))
else:
    raise RuntimeError(cmd)
p.write_text(json.dumps(s))
if output:
    print(output)
sys.exit(rc)
'''


class FailoverTests(unittest.TestCase):
    def run_monitor(self, scenario, *, route_failure=False, stale=False):
        self.assertTrue(WORKER.exists(), "tunnel health monitor is not implemented")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            bindir = root / "bin"
            bindir.mkdir()
            for cmd in ("ip", "curl", "sleep", "logger"):
                path = bindir / cmd
                path.write_text(FAKE_COMMAND)
                path.chmod(0o755)
            state = root / "state.json"
            main = ["default via 192.0.2.1 dev uplink metric 2"]
            state.write_text(json.dumps({
                "scenario": scenario, "cycle": 0, "commands": [], "probes": [],
                "samples": [], "logs": [], "route_failure": route_failure,
                "tables": {"main": main, "51820": ["default dev wg0 src 10.10.10.1 metric 10 proto 186"] if stale else []},
                "rules": ["priority 900 lookup unrelated"],
            }))
            result = subprocess.run(
                ["sh", str(WORKER), "run"],
                env={**os.environ, "PATH": f"{bindir}:{os.environ['PATH']}",
                     "SIM_STATE": str(state), "CELLBRIDGE_RUN_DIR": str(root / "run")},
                capture_output=True, text=True, timeout=20,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            data = json.loads(state.read_text())
            self.assertEqual(data["tables"]["main"], main, "must preserve physical routes")
            self.assertEqual(data["rules"], ["priority 900 lookup unrelated"])
            self.assertEqual(data["tables"].get("51820", []), [])
            self.assertEqual(data["tables"].get("51821", []), [])
            return data

    @staticmethod
    def selected(data):
        return [bool(sample.get("51820")) for sample in data["samples"]]

    def test_falls_back_after_three_failures_and_recovers_after_two_successes(self):
        # Missing withdrawal or recovery leaves clients on a dead path.
        data = self.run_monitor([False, True, True, False, False, False, True, True])
        self.assertEqual(self.selected(data), [False, False, True, True, True, False, False, True])

    def test_transient_failure_does_not_flap(self):
        data = self.run_monitor([True, True, False, False, True])
        self.assertEqual(self.selected(data), [False, True, True, True, True])

    def test_captive_portal_redirect_is_not_tunnel_internet(self):
        data = self.run_monitor(["portal", "portal", "portal"])
        self.assertEqual(self.selected(data), [False, False, False])

    def test_secondary_probe_target_can_establish_health(self):
        data = self.run_monitor(["secondary", "secondary"])
        self.assertEqual(self.selected(data), [False, True])

    def test_probe_commands_bind_tunnel_and_install_guard_during_fallback(self):
        data = self.run_monitor([False, False, True, True])
        for probe in data["probes"]:
            self.assertEqual(probe[probe.index("--interface") + 1], "wg0")
            self.assertIn("--ipv4", probe)
            self.assertEqual(probe[probe.index("--noproxy") + 1], "*")
            self.assertNotIn("--insecure", probe)
        for sample in data["samples"]:
            self.assertIn("unreachable default metric 32767 proto 186", sample["51821"])
            self.assertIn("default dev wg0 src 10.10.10.1 metric 10 proto 186", sample["51821"])
        self.assertIn(
            ["rule", "add", "priority", "0", "from", "10.10.10.1/32", "lookup", "51821", "protocol", "186"],
            data["commands"],
        )

    def test_route_install_failure_does_not_select_tunnel(self):
        data = self.run_monitor([True, True, True], route_failure=True)
        self.assertEqual(self.selected(data), [False, False, False])

    def test_restart_clears_stale_preferred_route_before_testing(self):
        data = self.run_monitor([False, True], stale=True)
        self.assertEqual(self.selected(data), [False, False])


if __name__ == "__main__":
    unittest.main()
