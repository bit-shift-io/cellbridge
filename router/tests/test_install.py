"""Sandbox the complete setup/revert scripts; never execute router operations.

Only filesystem roots and external OpenWrt commands are replaced. Config files
use JSON fixtures understood by the UCI boundary double, so backups/restores use
real files and can be checked byte-for-byte.
"""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROUTER = Path(__file__).resolve().parents[1]
KEY = "A" * 43 + "="
PHONE_PRIVATE = "dwdtCnMYpX08FsFyUbJmRd9ML4frwJkqsXf7pR25LCo="
PHONE_PUBLIC = "hSDwCYkwp1R0i33ctD73Wg2/Og0mOBr066SpjqqbTmo="

OPENWRT_COMMAND = r'''#!/usr/bin/env python3
import base64, hashlib, json, os, pathlib, re, subprocess, sys
root = pathlib.Path(os.environ["SANDBOX"])
cmd, args = pathlib.Path(sys.argv[0]).name, sys.argv[1:]
with (root / "events").open("a") as out:
    out.write(cmd + " " + " ".join(args) + "\n")
cfgdir = root / "etc/config"
if cmd == "uci":
    while args and args[0].startswith("-"):
        option = args.pop(0)
        if option == "-c":
            cfgdir = pathlib.Path(args.pop(0))
    op = args.pop(0)
    if op == "batch":
        # Keys are passed on stdin, as in real setup (not logged here).
        line = sys.stdin.read().strip()
        op, value = line.split(" ", 1)
        args = [value.replace("'", "")]
    if op in ("commit", "revert"):
        sys.exit(0)
    path, _, value = args[0].partition("=")
    parts = path.split(".")
    name = parts.pop(0)
    file = cfgdir / name
    data = json.loads(file.read_text()) if file.exists() else {}
    def resolve(section):
        match = re.fullmatch(r"@(\w+)\[(\d+)\]", section)
        if not match:
            return section
        matches = [k for k, v in data.items() if v["_type"] == match[1]]
        try:
            return matches[int(match[2])]
        except IndexError:
            sys.exit(1)
    sec = resolve(parts[0]) if parts else None
    field = parts[1] if len(parts) > 1 else "_type"
    if op == "get":
        try:
            got = data[sec][field]
        except KeyError:
            sys.exit(1)
        print(" ".join(got) if isinstance(got, list) else got)
    elif op == "show":
        for s, fields in data.items():
            print(f"{name}.{s}={fields['_type']}")
            for f, v in fields.items():
                if f != "_type":
                    print(f"{name}.{s}.{f}=" + repr(" ".join(v) if isinstance(v, list) else v))
    elif op == "add":
        sec = "cfg" + str(len(data))
        while sec in data:
            sec += "x"
        data[sec] = {"_type": args[1]}
        print(sec)
    elif op == "set":
        data.setdefault(sec, {})[field] = value
    elif op == "delete":
        try:
            if field == "_type":
                del data[sec]
            else:
                del data[sec][field]
        except KeyError:
            sys.exit(1)
    elif op == "add_list":
        old = data[sec].get(field, [])
        data[sec][field] = (old if isinstance(old, list) else [old]) + [value]
    elif op == "del_list":
        data[sec][field].remove(value)
    else:
        raise RuntimeError((op, args))
    if op not in ("get", "show"):
        file.write_text(json.dumps(data))
elif cmd == "cp":
    if os.environ.get("FAIL_BACKUP_COPY") == "1" and "cellbridge-backup/network" in args[-1]:
        pathlib.Path(args[-1]).write_text("partial copy")
        sys.exit(1)
    sys.exit(subprocess.run(["/usr/bin/cp", *args]).returncode)
elif cmd == "ip":
    if args == ["-Version"]:
        print("ip utility, iproute2-test")
    elif "rule" in args and "del" in args:
        sys.exit(2)
    elif args[:2] == ["link", "show"]:
        data = json.loads((cfgdir / "network").read_text())
        if "wg0" not in data:
            sys.exit(1)
        down = (root / "wg0-down").exists()
        flags = "POINTOPOINT,NOARP" + ("" if down else ",UP,LOWER_UP")
        state = "DOWN" if down else "UNKNOWN"
        print(f"44: wg0: <{flags}> mtu 1420 qdisc noqueue state {state} mode DEFAULT group default qlen 1000")
    elif args[:3] == ["-4", "addr", "show"]:
        print("inet 10.10.10.1/24")
    elif "show" in args and "table" in args:
        if os.environ.get("ROUTE_INSPECTION_FAILURE") == "1":
            sys.exit(1)
        if os.environ.get("ROUTE_CONFLICT") == "1":
            print("default via 192.0.2.1 dev uplink table 51820 proto 4")
elif cmd == "wg":
    if args[0] == "genkey":
        count_file = root / "genkey-count"
        count = int(count_file.read_text()) + 1 if count_file.exists() else 1
        count_file.write_text(str(count))
        print("dwdtCnMYpX08FsFyUbJmRd9ML4frwJkqsXf7pR25LCo=" if count == 1 else base64.b64encode(bytes([count]) * 32).decode())
    elif args[0] == "genpsk":
        print("A" * 43 + "=")
    elif args[0] == "pubkey":
        private = sys.stdin.read().strip()
        try:
            raw = base64.b64decode(private, validate=True)
            assert len(raw) == 32
        except Exception:
            sys.exit(1)
        print("hSDwCYkwp1R0i33ctD73Wg2/Og0mOBr066SpjqqbTmo=" if private == "dwdtCnMYpX08FsFyUbJmRd9ML4frwJkqsXf7pR25LCo=" else base64.b64encode(hashlib.sha256(raw).digest()).decode())
    elif args[0] == "show":
        data = json.loads((cfgdir / "network").read_text()) if (cfgdir / "network").exists() else {}
        wireguard = [name for name, section in data.items()
                     if section.get("_type") == "interface" and section.get("proto") == "wireguard"]
        if len(args) >= 2 and args[1] == "interfaces":
            print("\n".join(wireguard))
            sys.exit(0)
        iface = args[1] if len(args) >= 2 else None
        field = args[2] if len(args) >= 3 else None
        if field == "listen-port":
            print(data.get(iface, {}).get("listen_port", "0"))
            sys.exit(0)
        print("B" * 43 + "=")
elif cmd == "id":
    print("0")
elif cmd == "qrencode":
    (root / "qr-payload").write_text(sys.stdin.read())
    if os.environ.get("FAIL_QR_RENDER") == "1":
        sys.exit(1)
    print("[QR]")
elif cmd == "opkg":
    sys.exit(1)  # Tests use existing dependencies; no installation is allowed.
elif cmd == "fw4":
    if os.environ.get("FAIL_FIREWALL_RELOAD") == "1":
        sys.exit(1)
elif cmd == "ifstatus":
    if os.environ.get("FAIL_IFSTATUS") == "1":
        sys.exit(1)
    up = args[0] in os.environ.get("KMWAN_UP_INTERFACES", "wan").split()
    print(json.dumps({"up": up}))
elif cmd == "jsonfilter":
    data = json.load(sys.stdin)
    assert args == ["-e", "@.up"]
    print("true" if data["up"] else "false")
elif cmd == "ifup":
    if args[:1] == ["wg0"]:
        (root / "wg0-ifup").write_text(" ".join(args))
        (root / "wg0-down").unlink(missing_ok=True)
elif cmd in ("ifdown", "modprobe", "curl", "logger", "logread", "sleep"):
    pass
else:
    raise RuntimeError((cmd, args))
'''


class InstallationTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        for directory in ("etc/config", "etc/init.d", "etc/ssl/certs", "usr/sbin", "input", "bin", "var/run"):
            (self.root / directory).mkdir(parents=True, exist_ok=True)
        originals = {
            "network": {
                "lan": {"_type": "interface", "proto": "static", "ipaddr": "192.0.2.1"},
                "wan": {"_type": "interface", "proto": "dhcp", "metric": "1"},
                "tethering": {"_type": "interface", "proto": "dhcp", "metric": "9"},
            },
            "firewall": {"lan_zone": {"_type": "zone", "name": "lan", "input": "ACCEPT"}},
            "dhcp": {"dns": {"_type": "dnsmasq", "server": ["192.0.2.53"]}},
            "kmwan": {
                "global": {"_type": "global", "enable": "1", "mode": "failover"},
                "usb": {"_type": "member", "interface": "tethering", "addr_type": "4", "metric": "9"},
                "wired": {"_type": "member", "interface": "wan", "addr_type": "4", "metric": "1"},
            },
        }
        self.originals = {}
        for name, value in originals.items():
            data = json.dumps(value).encode()
            (self.root / "etc/config" / name).write_bytes(data)
            self.originals[name] = data
        (self.root / "etc/ssl/certs/ca-certificates.crt").write_text("fixture CA bundle")
        self.env = {**os.environ, "SANDBOX": str(self.root),
                    "PATH": f"{self.root / 'bin'}:{os.environ['PATH']}"}
        for cmd in ("uci", "cp", "ip", "wg", "id", "qrencode", "opkg", "ifup", "ifdown", "fw4", "modprobe", "curl", "logger", "logread", "ifstatus", "jsonfilter", "sleep"):
            path = self.root / "bin" / cmd
            path.write_text(OPENWRT_COMMAND)
            path.chmod(0o755)
        for service in ("network", "dnsmasq", "kmwan", "firewall"):
            path = self.root / "etc/init.d" / service
            path.write_text('''#!/bin/sh
printf '%s %s\n' "$0" "$*" >> "$SANDBOX/events"
case "$0:$1" in
 */kmwan:restart)
  test "${FAIL_KMWAN_RESTART:-0}" != 1 || exit 1
  test "${KMWAN_RESTART_EXIT:-0}" = 0 || exit "$KMWAN_RESTART_EXIT"
  if test "${KMWAN_IPV6_EXIT_QUIRK:-0}" = 1; then
   ipv6_enabled=$(uci -q get glipv6.globals.enabled)
   # Reproduce the vendor tail: an implicit exit preserves this false test.
   [ "$ipv6_enabled" = 1 ] && :
   exit
  fi
  ;;
esac
''')
            path.chmod(0o755)
        # Model the procd command boundary, not its own implementation.
        rc = self.root / "etc/rc.common"
        rc.write_text('''#!/bin/sh
printf 'service %s %s\n' "$1" "$2" >> "$SANDBOX/events"
case "$2" in
 enable) touch "$SANDBOX/service-enabled" ;;
 disable) rm -f "$SANDBOX/service-enabled" ;;
 enabled) test -f "$SANDBOX/service-enabled" ;;
 start) touch "$SANDBOX/service-running" ;;
 running) test -f "$SANDBOX/service-running" ;;
 stop)
  rm -f "$SANDBOX/service-running"
  . "$1"
  if command -v service_stopped >/dev/null 2>&1; then service_stopped; fi
  ;;
esac
''')
        rc.chmod(0o755)
        for file in ROUTER.glob("cellbridge-*.sh"):
            text = file.read_text()
            for prefix in ("/etc/", "/usr/sbin/", "/var/run/", "/proc/gl-kmwan/", "/proc/net/"):
                text = text.replace(prefix, f"{self.root}{prefix}")
            target = self.root / "input" / file.name
            target.write_text(text)
            target.chmod(0o755)

    def run_script(self, name, *args, success=True):
        result = subprocess.run(["sh", str(self.root / "input" / name), *args],
                                env=self.env, capture_output=True, text=True, timeout=20)
        if success:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0)
        return result

    def setup_router(self):
        self.run_script("cellbridge-setup.sh", "--phone-public-key", KEY)

    def config(self, name):
        return json.loads((self.root / "etc/config" / name).read_text())

    def add_wireguard_interface(self, name, port):
        network = self.config("network")
        network[name] = {
            "_type": "interface",
            "proto": "wireguard",
            "listen_port": str(port),
        }
        (self.root / "etc/config/network").write_text(json.dumps(network))

    def add_udp_listener(self, port):
        proc = self.root / "proc/net"
        proc.mkdir(parents=True, exist_ok=True)
        (proc / "udp").write_text(
            "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n"
            f"   0: 00000000:{port:04X} 00000000:0000 07 00000000:00000000 00:00000000 00000000     0        0 0 1 0000000000000000 0\n"
        )

    def test_setup_refuses_port_used_by_another_wireguard_interface_before_changes(self):
        self.add_wireguard_interface("wgserver", 51820)
        self.add_udp_listener(51820)
        result = self.run_script("cellbridge-setup.sh", success=False)
        self.assertIn("wgserver", result.stderr)
        self.assertIn("--port", result.stderr)
        self.assertFalse((self.root / "etc/cellbridge-backup").exists())
        self.assertFalse((self.root / "service-running").exists())

    def test_setup_refuses_occupied_udp_port_without_named_wireguard_owner(self):
        self.add_udp_listener(51820)
        result = self.run_script("cellbridge-setup.sh", success=False)
        self.assertIn("UDP 51820", result.stderr)
        self.assertIn("--port", result.stderr)
        self.assertFalse((self.root / "etc/cellbridge-backup").exists())

    def test_explicit_free_port_updates_config_and_qr(self):
        self.add_wireguard_interface("wgserver", 51820)
        self.add_udp_listener(51820)
        self.run_script("cellbridge-setup.sh", "--phone-public-key", KEY, "--port", "51830")
        self.assertEqual(self.config("network")["wg0"]["listen_port"], "51830")
        self.assertEqual(self.qr_payload()["endpoint"], "192.0.2.1:51830")

    def test_rerun_reuses_the_existing_wg0_port_even_while_it_is_listening(self):
        self.add_wireguard_interface("wgserver", 51820)
        self.add_udp_listener(51820)
        self.run_script("cellbridge-setup.sh", "--phone-public-key", KEY, "--port", "51830")
        self.add_udp_listener(51830)
        self.run_script("cellbridge-setup.sh", "--phone-public-key", KEY)
        self.assertEqual(self.config("network")["wg0"]["listen_port"], "51830")
        self.assertEqual(self.qr_payload()["endpoint"], "192.0.2.1:51830")

    def test_setup_brings_up_an_addressed_but_down_interface(self):
        (self.root / "wg0-down").touch()
        self.run_script("cellbridge-setup.sh", "--phone-public-key", KEY)
        self.assertEqual((self.root / "wg0-ifup").read_text(), "wg0")

    def test_invalid_port_is_rejected_before_changes(self):
        result = self.run_script("cellbridge-setup.sh", "--port", "70000", success=False)
        self.assertIn("--port", result.stderr)
        self.assertFalse((self.root / "etc/cellbridge-backup").exists())

    def test_setup_preserves_first_backup_and_revert_restores_every_configuration(self):
        self.setup_router()
        self.assertTrue((self.root / "service-enabled").exists())
        for name, data in self.originals.items():
            backup = self.root / "etc/cellbridge-backup" / name
            self.assertEqual(backup.read_bytes(), data)
            self.assertEqual(backup.stat().st_mode & 0o077, 0)
        self.assertEqual(self.config("network")["wan"].get("disabled", "0"), "0")
        self.assertEqual(self.config("network")["tethering"]["metric"], "1")
        self.assertEqual(self.config("network")["wan"]["metric"], "2")
        self.assertEqual(self.config("kmwan")["global"]["enable"], "1")
        self.assertEqual(self.config("kmwan")["usb"]["metric"], "1")
        self.assertEqual(self.config("kmwan")["wired"]["metric"], "2")
        peers = [v for v in self.config("network").values() if v["_type"] == "wireguard_wg0"]
        self.assertEqual(len(peers), 1)
        self.assertEqual(peers[0]["route_allowed_ips"], "0")
        self.setup_router()
        for name, data in self.originals.items():
            self.assertEqual((self.root / "etc/cellbridge-backup" / name).read_bytes(), data)
        self.run_script("cellbridge-revert.sh")
        for name, data in self.originals.items():
            self.assertEqual((self.root / "etc/config" / name).read_bytes(), data)
        self.assertFalse((self.root / "service-enabled").exists())
        self.assertFalse((self.root / "etc/init.d/cellbridge-failover").exists())
        self.assertFalse((self.root / "usr/sbin/cellbridge-failover").exists())
        self.assertFalse((self.root / "etc/cellbridge-backup").exists())
        before = {n: (self.root / "etc/config" / n).read_bytes() for n in self.originals}
        self.run_script("cellbridge-revert.sh")
        self.assertEqual({n: (self.root / "etc/config" / n).read_bytes() for n in before}, before)

    def test_missing_companion_aborts_before_router_configuration_changes(self):
        (self.root / "input/cellbridge-failover.sh").unlink(missing_ok=True)
        self.run_script("cellbridge-setup.sh", "--phone-public-key", KEY, success=False)
        self.assertFalse((self.root / "etc/cellbridge-backup").exists())
        for name, data in self.originals.items():
            self.assertEqual((self.root / "etc/config" / name).read_bytes(), data)

    def test_foreign_routing_table_aborts_before_configuration_changes(self):
        self.env["ROUTE_CONFLICT"] = "1"
        self.run_script("cellbridge-setup.sh", "--phone-public-key", KEY, success=False)
        for name, data in self.originals.items():
            self.assertEqual((self.root / "etc/config" / name).read_bytes(), data)

    def test_lost_completed_backup_cannot_be_replaced_with_modified_configuration(self):
        self.setup_router()
        backup = self.root / "etc/cellbridge-backup"
        (backup / "network").unlink()
        self.run_script("cellbridge-setup.sh", "--phone-public-key", KEY, success=False)
        self.assertFalse((backup / "network").exists())
        self.assertTrue((backup / "snapshot-complete").exists())

    def test_missing_kmwan_backup_refuses_guessed_restore(self):
        self.setup_router()
        backup = self.root / "etc/cellbridge-backup"
        (backup / "kmwan").unlink()
        before = (self.root / "etc/config/network").read_bytes()
        self.run_script("cellbridge-revert.sh", success=False)
        self.assertEqual((self.root / "etc/config/network").read_bytes(), before)
        self.assertTrue((backup / "snapshot-complete").exists())
        self.assertTrue((self.root / "etc/init.d/cellbridge-failover").exists())

    def test_failed_service_restore_keeps_backups_for_retry(self):
        self.setup_router()
        self.env["FAIL_KMWAN_RESTART"] = "1"
        self.run_script("cellbridge-revert.sh", success=False)
        self.assertTrue((self.root / "etc/cellbridge-backup/network").exists())
        self.env.pop("FAIL_KMWAN_RESTART")
        self.run_script("cellbridge-revert.sh")
        self.assertFalse((self.root / "etc/cellbridge-backup").exists())

    def test_existing_managed_files_and_service_enabled_state_are_restored(self):
        worker = self.root / "usr/sbin/cellbridge-failover"
        init = self.root / "etc/init.d/cellbridge-failover"
        worker_data = b"#!/bin/sh\n# CellBridge-managed previous version\nexit 0\n"
        init_data = f"#!{self.root}/etc/rc.common\n# CellBridge-managed previous service\n".encode()
        worker.write_bytes(worker_data)
        init.write_bytes(init_data)
        worker.chmod(0o700)
        init.chmod(0o700)
        (self.root / "service-enabled").touch()
        self.setup_router()
        self.run_script("cellbridge-revert.sh")
        self.assertEqual(worker.read_bytes(), worker_data)
        self.assertEqual(init.read_bytes(), init_data)
        self.assertTrue((self.root / "service-enabled").exists())
        self.assertFalse((self.root / "service-running").exists())
        self.run_script("cellbridge-revert.sh")
        self.assertEqual(worker.read_bytes(), worker_data)
        self.assertEqual(init.read_bytes(), init_data)

    def test_failed_copy_cannot_leave_a_reusable_partial_snapshot(self):
        self.env["FAIL_BACKUP_COPY"] = "1"
        self.run_script("cellbridge-setup.sh", "--phone-public-key", KEY, success=False)
        backup = self.root / "etc/cellbridge-backup"
        self.assertFalse((backup / "network").exists())
        self.assertFalse((backup / "snapshot-complete").exists())
        for name, data in self.originals.items():
            self.assertEqual((self.root / "etc/config" / name).read_bytes(), data)
        self.env.pop("FAIL_BACKUP_COPY")
        # Incomplete new-format snapshots must be repaired explicitly, never
        # filled from potentially modified live files on a later invocation.
        self.run_script("cellbridge-setup.sh", "--phone-public-key", KEY, success=False)

    def test_incomplete_snapshot_does_not_authorize_removal_of_original_files(self):
        backup = self.root / "etc/cellbridge-backup"
        backup.mkdir()
        for name in ("network", "firewall", "dhcp"):
            (backup / name).write_bytes(self.originals[name])
        worker = self.root / "usr/sbin/cellbridge-failover"
        original = b"#!/bin/sh\n# CellBridge-managed original\nexit 0\n"
        worker.write_bytes(original)
        worker.chmod(0o700)
        self.run_script("cellbridge-revert.sh", success=False)
        self.assertEqual(worker.read_bytes(), original)
        self.assertTrue(backup.exists())

    def test_missing_snapshot_metadata_cannot_recapture_installed_monitor(self):
        self.setup_router()
        backup = self.root / "etc/cellbridge-backup"
        (backup / "managed-files-snapshot").unlink()
        self.run_script("cellbridge-setup.sh", "--phone-public-key", KEY, success=False)
        self.assertFalse((backup / "managed-files-snapshot").exists())

    def test_missing_installed_worker_does_not_block_revert(self):
        self.setup_router()
        (self.root / "usr/sbin/cellbridge-failover").unlink()
        self.run_script("cellbridge-revert.sh")
        self.assertFalse((self.root / "etc/init.d/cellbridge-failover").exists())
        for name, data in self.originals.items():
            self.assertEqual((self.root / "etc/config" / name).read_bytes(), data)

    def test_firewall_restore_failure_keeps_snapshot(self):
        self.setup_router()
        self.env["FAIL_FIREWALL_RELOAD"] = "1"
        self.run_script("cellbridge-revert.sh", success=False)
        self.assertTrue((self.root / "etc/cellbridge-backup/network").exists())

    def test_existing_monitor_does_not_authorize_foreign_table_cleanup(self):
        worker = self.root / "usr/sbin/cellbridge-failover"
        worker.write_text("#!/bin/sh\n# CellBridge-managed original\nexit 0\n")
        worker.chmod(0o700)
        self.env["ROUTE_CONFLICT"] = "1"
        self.run_script("cellbridge-setup.sh", "--phone-public-key", KEY, success=False)
        for name, data in self.originals.items():
            self.assertEqual((self.root / "etc/config" / name).read_bytes(), data)

    def test_failed_routing_inspection_does_not_allow_installation(self):
        self.env["ROUTE_INSPECTION_FAILURE"] = "1"
        self.run_script("cellbridge-setup.sh", "--phone-public-key", KEY, success=False)
        for name, data in self.originals.items():
            self.assertEqual((self.root / "etc/config" / name).read_bytes(), data)

    def test_disabled_but_running_original_service_is_restarted_without_enabling(self):
        worker = self.root / "usr/sbin/cellbridge-failover"
        init = self.root / "etc/init.d/cellbridge-failover"
        worker.write_text("#!/bin/sh\n# CellBridge-managed original\nexit 0\n")
        init.write_text(f"#!{self.root}/etc/rc.common\n# CellBridge-managed original\n")
        worker.chmod(0o700)
        init.chmod(0o700)
        (self.root / "service-running").touch()
        self.setup_router()
        self.run_script("cellbridge-revert.sh")
        self.assertFalse((self.root / "service-enabled").exists())
        self.assertTrue((self.root / "service-running").exists())

    def qr_payload(self):
        return json.loads((self.root / "qr-payload").read_text().removeprefix("CELLBRIDGE1:"))

    def test_default_setup_generates_phone_keys_once_and_revert_removes_them(self):
        result = self.run_script("cellbridge-setup.sh")
        payload = self.qr_payload()
        self.assertEqual(payload["phonePrivateKey"], PHONE_PRIVATE)
        self.assertEqual(payload["phonePublicKey"], PHONE_PUBLIC)
        peers = [v for v in self.config("network").values() if v["_type"] == "wireguard_wg0"]
        self.assertEqual(peers[0]["public_key"], PHONE_PUBLIC)
        self.assertEqual(peers[0]["cellbridge_phone_private_key"], PHONE_PRIVATE)
        self.assertNotEqual(self.config("network")["wg0"]["private_key"], PHONE_PRIVATE)
        self.assertEqual((self.root / "etc/config/network").stat().st_mode & 0o077, 0)
        self.assertNotIn(PHONE_PRIVATE, result.stdout + result.stderr)
        self.assertNotIn(PHONE_PRIVATE, (self.root / "events").read_text())
        self.run_script("cellbridge-setup.sh")
        self.assertEqual(self.qr_payload(), payload)
        self.assertEqual((self.root / "genkey-count").read_text(), "2")
        self.assertEqual((self.root / "etc/cellbridge-backup/network").read_bytes(), self.originals["network"])
        self.run_script("cellbridge-revert.sh")
        self.assertEqual((self.root / "etc/config/network").read_bytes(), self.originals["network"])

    def test_explicit_phone_public_key_keeps_phone_managed_key_flow(self):
        self.setup_router()
        self.assertNotIn("phonePrivateKey", self.qr_payload())
        self.assertEqual(self.qr_payload()["phonePublicKey"], KEY)
        peers = [v for v in self.config("network").values() if v["_type"] == "wireguard_wg0"]
        self.assertNotIn("cellbridge_phone_private_key", peers[0])

    def test_corrupt_stored_phone_key_aborts_instead_of_rotating_it(self):
        network = self.config("network")
        network["phone"] = {"_type": "wireguard_wg0", "public_key": PHONE_PUBLIC,
                            "cellbridge_phone_private_key": "invalid-secret"}
        path = self.root / "etc/config/network"
        path.write_text(json.dumps(network))
        before = path.read_bytes()
        result = self.run_script("cellbridge-setup.sh", success=False)
        self.assertEqual(path.read_bytes(), before)
        self.assertNotIn("invalid-secret", result.stdout + result.stderr)
        self.assertFalse((self.root / "genkey-count").exists())

    def test_stored_generated_pair_must_match_and_is_restored_from_backup(self):
        network = self.config("network")
        network["phone"] = {"_type": "wireguard_wg0", "public_key": KEY,
                            "cellbridge_phone_private_key": PHONE_PRIVATE}
        path = self.root / "etc/config/network"
        path.write_text(json.dumps(network))
        before = path.read_bytes()
        self.run_script("cellbridge-setup.sh", success=False)
        self.assertEqual(path.read_bytes(), before)

    def test_revert_preserves_original_generated_phone_key_metadata(self):
        network = self.config("network")
        network["phone"] = {"_type": "wireguard_wg0", "public_key": PHONE_PUBLIC,
                            "cellbridge_phone_private_key": PHONE_PRIVATE}
        path = self.root / "etc/config/network"
        path.write_text(json.dumps(network))
        original = path.read_bytes()
        self.run_script("cellbridge-setup.sh")
        self.assertEqual(self.qr_payload()["phonePrivateKey"], PHONE_PRIVATE)
        self.run_script("cellbridge-revert.sh")
        self.assertEqual(path.read_bytes(), original)

    def test_switching_generated_keys_to_manual_mode_removes_private_qr_metadata(self):
        self.run_script("cellbridge-setup.sh")
        self.setup_router()
        self.assertNotIn("phonePrivateKey", self.qr_payload())
        self.assertEqual(self.qr_payload()["phonePublicKey"], KEY)
        peers = [v for v in self.config("network").values() if v["_type"] == "wireguard_wg0"]
        self.assertNotIn("cellbridge_phone_private_key", peers[0])
        self.assertEqual((self.root / "genkey-count").read_text(), "2")

    def test_failed_qr_renderer_prints_matching_raw_payload_without_argv_leaks(self):
        self.env["FAIL_QR_RENDER"] = "1"
        result = self.run_script("cellbridge-setup.sh")
        raw = next(line for line in result.stdout.splitlines() if line.startswith("CELLBRIDGE1:"))
        payload = json.loads(raw.removeprefix("CELLBRIDGE1:"))
        self.assertEqual(payload["phonePrivateKey"], PHONE_PRIVATE)
        self.assertEqual(payload["phonePublicKey"], PHONE_PUBLIC)
        self.assertNotIn(PHONE_PRIVATE, result.stderr)
        self.assertNotIn(PHONE_PRIVATE, (self.root / "events").read_text())

    def configure_native_restart_quirk(self, *, runtime="wan:online\n", up="wan"):
        (self.root / "etc/config/glipv6").write_text(json.dumps({
            "globals": {"_type": "globals", "enabled": "0"},
        }))
        proc = self.root / "proc/gl-kmwan"
        proc.mkdir(parents=True)
        (proc / "config").write_text(runtime)
        self.env["KMWAN_IPV6_EXIT_QUIRK"] = "1"
        self.env["KMWAN_UP_INTERFACES"] = up

    def test_vendor_ipv6_exit_quirk_allows_setup_and_exact_revert_when_runtime_ready(self):
        self.configure_native_restart_quirk()
        self.setup_router()
        self.assertTrue((self.root / "service-running").exists())
        self.assertEqual(self.qr_payload()["phonePublicKey"], KEY)
        self.run_script("cellbridge-revert.sh")
        for name, data in self.originals.items():
            self.assertEqual((self.root / "etc/config" / name).read_bytes(), data)
        self.assertFalse((self.root / "etc/cellbridge-backup").exists())

    def test_exit_quirk_allows_disconnected_backups_without_claiming_internet(self):
        self.configure_native_restart_quirk(runtime="", up="")
        self.setup_router()
        self.assertTrue((self.root / "service-running").exists())

    def test_exit_quirk_does_not_mask_missing_up_interface_registration(self):
        self.configure_native_restart_quirk(runtime="tethering:online\n", up="wan")
        self.run_script("cellbridge-setup.sh", "--phone-public-key", KEY, success=False)
        self.assertFalse((self.root / "qr-payload").exists())
        self.assertFalse((self.root / "service-running").exists())
        self.assertTrue((self.root / "etc/cellbridge-backup/network").exists())

    def test_exit_quirk_does_not_mask_unavailable_native_engine(self):
        self.configure_native_restart_quirk()
        (self.root / "proc/gl-kmwan/config").unlink()
        self.run_script("cellbridge-setup.sh", "--phone-public-key", KEY, success=False)
        self.assertFalse((self.root / "service-running").exists())

    def test_non_quirk_exit_code_is_fatal_even_with_ready_runtime(self):
        self.configure_native_restart_quirk()
        self.env["KMWAN_RESTART_EXIT"] = "2"
        self.run_script("cellbridge-setup.sh", "--phone-public-key", KEY, success=False)
        self.assertFalse((self.root / "service-running").exists())

    def test_exit_quirk_does_not_mask_failed_status_inspection_of_configured_interfaces(self):
        self.configure_native_restart_quirk()
        self.env["FAIL_IFSTATUS"] = "1"
        self.run_script("cellbridge-setup.sh", "--phone-public-key", KEY, success=False)
        self.assertFalse((self.root / "service-running").exists())

    def test_retry_after_native_restart_failure_reuses_keys_and_original_snapshot(self):
        self.env["FAIL_KMWAN_RESTART"] = "1"
        self.run_script("cellbridge-setup.sh", success=False)
        peers = [v for v in self.config("network").values() if v["_type"] == "wireguard_wg0"]
        original_phone_key = peers[0]["cellbridge_phone_private_key"]
        self.assertFalse((self.root / "qr-payload").exists())
        self.env.pop("FAIL_KMWAN_RESTART")
        self.configure_native_restart_quirk()
        self.run_script("cellbridge-setup.sh")
        self.assertEqual(self.qr_payload()["phonePrivateKey"], original_phone_key)
        self.assertEqual((self.root / "genkey-count").read_text(), "2")
        self.assertEqual((self.root / "etc/cellbridge-backup/network").read_bytes(), self.originals["network"])

    def test_revert_keeps_original_snapshot_if_quirk_runtime_validation_fails(self):
        self.setup_router()
        self.configure_native_restart_quirk(runtime="", up="wan")
        self.run_script("cellbridge-revert.sh", success=False)
        self.assertTrue((self.root / "etc/cellbridge-backup/network").exists())


if __name__ == "__main__":
    unittest.main()
