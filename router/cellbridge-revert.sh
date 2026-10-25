#!/bin/sh
# CellBridge router revert for the GL.iNet Flint 3 (GL-BE9300), OpenWrt + fw4.
#
# Undoes cellbridge-setup.sh:
#   * restores /etc/config/{network,firewall,dhcp} from the snapshot taken by
#     the setup script (/etc/cellbridge-backup), or
#   * if no snapshot exists, removes what setup added (wg0, its peer,
#     the "vpn" zone, the lan->vpn forwarding, the DNS override) and re-enables
#     the WAN interface(s).
# Generated phone private keys live only in network peer metadata: exact network
# restore preserves original keys, and targeted peer removal removes new keys.
#
# The snapshot is the *only* exact record of the previous state.  This script
# therefore keeps it when a restore fails, and only deletes it after a
# successful restore.
#
# Stops/removes the tunnel health monitor and its policy routing, restores
# original kmwan priorities as well as network/firewall/DNS. Run over LAN SSH
# (ssh root@192.168.8.1), which stays reachable in either state.
#
# Usage:
#   sh cellbridge-revert.sh
#
# POSIX sh / BusyBox ash only.  Idempotent and safe to run twice, and safe on
# a router where setup never ran.

set -u

STATE_DIR=/etc/cellbridge-backup
WG_IF=wg0
WG_LISTEN_PORT=51820
WG_ADDR=10.10.10.1/24
DEFAULT_DNS=1.1.1.1
MONITOR=/usr/sbin/cellbridge-failover
MONITOR_INIT=/etc/init.d/cellbridge-failover
INSTALL_MARKER=/etc/cellbridge-installed
RESTORED_MARKER=/etc/cellbridge-restored
SCRIPT_DIR=$(CDPATH= cd "$(dirname "$0")" && pwd) || exit 1

msg()  { printf '%s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die()  { printf 'error: %s\n' "$*" >&2; exit 1; }

usage() {
	cat <<'EOF'
CellBridge router revert

Usage:
  sh cellbridge-revert.sh

Restores the snapshot in /etc/cellbridge-backup, or removes the CellBridge
configuration if no snapshot exists. Stops tunnel health routing and restores
the original kmwan configuration and WAN uplinks.
EOF
}

case "${1:-}" in
	-h|--help) usage; exit 0 ;;
	"") : ;;
	*) usage >&2; die "unknown argument: $1" ;;
esac

[ "$(id -u 2>/dev/null)" = 0 ] || die "this script must run as root on the router"
command -v uci >/dev/null 2>&1 || die "uci not found; is this OpenWrt?"
command -v ip  >/dev/null 2>&1 || die "ip not found"

umask 077

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

# Does an /etc/config/network-style file contain `config interface '<name>'`?
config_has_interface() {
	[ -f "$1" ] || return 1
	grep -Eq "^[[:space:]]*config[[:space:]]+interface[[:space:]]+'$2'[[:space:]]*\$" "$1" ||
		grep -Eq "^[[:space:]]*config[[:space:]]+interface[[:space:]]+$2[[:space:]]*\$" "$1"
}

detect_wan_sections() {
	for sec in $(uci -q show network |
		awk -F'[.=]' '/=interface$/ { print $2 }'); do
		name=$(uci -q get "network.$sec.name")
		[ -n "$name" ] || name=$sec
		case "$name" in
			lan|loopback|"$WG_IF") continue ;;
		esac
		lname=$(printf '%s' "$name" | tr 'A-Z' 'a-z')
		case "$lname" in
			wan*|wwan*|usbwan*|*_wan) printf '%s\n' "$sec" ;;
		esac
	done
}

section_name() {
	# $1 = uci section path prefix (network.<sec>)
	n=$(uci -q get "$1.name")
	[ -n "$n" ] || n=${1##*.}
	printf '%s\n' "$n"
}

# WAN sections recorded at setup time, else detected now.
if [ -f "$STATE_DIR/wan-sections" ]; then
	WAN_SECTIONS=$(tr '\n' ' ' < "$STATE_DIR/wan-sections")
else
	WAN_SECTIONS=$(detect_wan_sections)
fi

print_wan_state() {
	msg ""
	msg "Current WAN state:"
	if [ -z "$WAN_SECTIONS" ]; then
		msg "  (no WAN interface detected)"
	else
		for w in $WAN_SECTIONS; do
			name=$(section_name "network.$w")
			dis=$(uci -q get "network.$w.disabled")
			[ -n "$dis" ] || dis=0
			if command -v ifstatus >/dev/null 2>&1; then
				if ifstatus "$name" 2>/dev/null |
					grep -q '"up":[[:space:]]*true'; then
					st=up
				else
					st=not-up
				fi
			else
				st=unknown
			fi
			msg "  $name: disabled=$dis ifstatus=$st"
		done
	fi
	msg "  default route(s):"
	ip route show 2>/dev/null | grep '^default' | sed 's/^/    /'
}

# Bring enabled WAN interfaces up without waiting for DHCP.  Harmless when
# they are already up; helps netifd after an interface was disabled.
bring_wan_up() {
	for w in $WAN_SECTIONS; do
		[ "$(uci -q get "network.$w.disabled")" = 1 ] && continue
		name=$(section_name "network.$w")
		ifup "$name" >/dev/null 2>&1 || true
	done
}

reload_services() {
	msg "Reloading network, firewall and dnsmasq"
	net_ok=1
	/etc/init.d/network reload >/dev/null 2>&1 || net_ok=0
	if command -v fw4 >/dev/null 2>&1; then
		fw4 reload >/dev/null 2>&1 || { warn "fw4 reload returned an error"; net_ok=0; }
	else
		/etc/init.d/firewall reload >/dev/null 2>&1 ||
			{ warn "firewall reload returned an error"; net_ok=0; }
	fi
	/etc/init.d/dnsmasq restart >/dev/null 2>&1 ||
		{ warn "dnsmasq restart returned an error"; net_ok=0; }
	[ "$net_ok" = 1 ] || warn "network reload returned an error"
	bring_wan_up
	if [ -x /etc/init.d/kmwan ]; then
		/etc/init.d/kmwan restart >/dev/null 2>&1 || {
			warn "kmwan restart failed; keeping backups for a retry"
			return 1
		}
	fi
	[ "$net_ok" = 1 ]
}

# Require a completed snapshot and all metadata/payloads for its format.
snapshot_is_usable() {
	[ -d "$STATE_DIR" ] || return 1
	[ -f "$STATE_DIR/snapshot-complete" ] || return 1
	if [ -f "$STATE_DIR/snapshot-version" ]; then
		[ "$(cat "$STATE_DIR/snapshot-version")" = 2 ] || return 1
		for item in kmwan-snapshot managed-files-snapshot failover-enabled failover-running; do
			[ -f "$STATE_DIR/$item" ] || return 1
		done
		[ "$(cat "$STATE_DIR/snapshot-complete")" = complete ] &&
			[ "$(cat "$STATE_DIR/managed-files-snapshot")" = complete ] || return 1
		case "$(cat "$STATE_DIR/kmwan-snapshot")" in present|absent) : ;; *) return 1 ;; esac
		for item in failover-enabled failover-running; do
			grep -qx '[01]' "$STATE_DIR/$item" || return 1
		done
	elif [ -e "$INSTALL_MARKER" ] || [ -e "$MONITOR" ] || [ -e "$MONITOR_INIT" ]; then
		return 1
	fi
	for cfg in network firewall dhcp; do
		[ -f "$STATE_DIR/$cfg" ] || return 1
	done
	if [ -f "$STATE_DIR/kmwan-snapshot" ] &&
		[ "$(cat "$STATE_DIR/kmwan-snapshot")" = present ]; then
		[ -f "$STATE_DIR/kmwan" ] || return 1
	fi
	if [ -f "$STATE_DIR/managed-files-snapshot" ]; then
		for item in failover-worker failover-init; do
			if [ -f "$STATE_DIR/$item" ] && [ -f "$STATE_DIR/$item-absent" ]; then return 1; fi
			[ -f "$STATE_DIR/$item" ] || [ -f "$STATE_DIR/$item-absent" ] || return 1
		done
		[ -f "$STATE_DIR/failover-enabled" ] || return 1
	fi
	return 0
}

stop_remove_failover() {
	owned=0
	for path in "$MONITOR" "$MONITOR_INIT"; do
		[ -e "$path" ] || continue
		grep -q 'CellBridge-managed' "$path" || {
			warn "refusing to remove an unrelated file: $path"
			return 1
		}
		owned=1
	done
	[ "$owned" = 1 ] || return 0
	# Prefer the companion supplied beside revert. It also recovers a partial
	# installation missing its installed worker, without guessing table ownership.
	cleanup_tool=$SCRIPT_DIR/cellbridge-failover.sh
	[ -f "$cleanup_tool" ] || cleanup_tool=$MONITOR
	[ -f "$cleanup_tool" ] && grep -q 'CellBridge-managed' "$cleanup_tool" || {
		warn "need cellbridge-failover.sh beside revert for safe routing cleanup"
		return 1
	}
	sh "$cleanup_tool" check || return 1
	if [ -x "$MONITOR_INIT" ]; then
		"$MONITOR_INIT" stop || return 1
		"$MONITOR_INIT" disable || return 1
	fi
	sh "$cleanup_tool" cleanup || return 1
	rm -f "$MONITOR" "$MONITOR_INIT" || return 1
	rm -f /var/run/cellbridge-failover/status /var/run/cellbridge-failover/status.new
	msg "Stopped and removed CellBridge tunnel health routing"
}

restore_managed_files() {
	[ -f "$STATE_DIR/managed-files-snapshot" ] || return 0
	for item in failover-worker failover-init; do
		case "$item" in
			failover-worker) path=$MONITOR ;;
			failover-init) path=$MONITOR_INIT ;;
		esac
		if [ -f "$STATE_DIR/$item" ]; then
			cp "$STATE_DIR/$item" "$path" || return 1
			chmod 700 "$path" || return 1
			cmp -s "$STATE_DIR/$item" "$path" || return 1
		fi
	done
	if [ -x "$MONITOR_INIT" ] && [ "$(cat "$STATE_DIR/failover-enabled")" = 1 ]; then
		"$MONITOR_INIT" enable || return 1
	fi
	if [ -x "$MONITOR_INIT" ] && [ "$(cat "$STATE_DIR/failover-running")" = 1 ]; then
		"$MONITOR_INIT" start || return 1
	fi
}

# Is network.wg0 the interface this project creates?  Requiring the tunnel
# address and port means a pre-existing, unrelated wg0 (for example restored
# from a snapshot) is never mistaken for ours on a later revert run.
is_cellbridge_wg0() {
	[ "$(uci -q get "network.$WG_IF.proto")" = wireguard ] || return 1
	[ "$(uci -q get "network.$WG_IF.listen_port")" = "$WG_LISTEN_PORT" ] ||
		return 1
	uci -q get "network.$WG_IF.addresses" | grep -q "$WG_ADDR"
}

# Are any CellBridge artifacts present?  Used to decide whether the targeted
# (no-snapshot) cleanup has anything to do.  This is what makes a second run
# after a successful restore a no-op instead of guessing.
has_artifacts() {
	for path in "$MONITOR" "$MONITOR_INIT"; do
		[ ! -f "$path" ] || ! grep -q 'CellBridge-managed' "$path" || return 0
	done
	is_cellbridge_wg0 && return 0

	# A wg0 that is not ours: leave it and its firewall references alone.
	[ -n "$(uci -q get "network.$WG_IF")" ] && return 1

	# No wg0 section at all: a zone/forwarding that still names wg0 is a
	# leftover from a previous partial cleanup.
	for sec in $(uci -q show firewall |
		awk -F'[.=]' '/=zone$/ { print $2 }'); do
		[ "$(uci -q get "firewall.$sec.name")" = vpn ] || continue
		for n in $(uci -q get "firewall.$sec.network"); do
			[ "$n" = "$WG_IF" ] && return 0
		done
	done

	for sec in $(uci -q show firewall |
		awk -F'[.=]' '/=forwarding$/ { print $2 }'); do
		[ "$(uci -q get "firewall.$sec.src")" = lan ] &&
			[ "$(uci -q get "firewall.$sec.dest")" = vpn ] &&
			return 0
	done

	return 1
}

# ---------------------------------------------------------------------------
# Exact restore from the snapshot
# ---------------------------------------------------------------------------

restore_snapshot() {
	had_wg0=0
	config_has_interface "$STATE_DIR/network" "$WG_IF" && had_wg0=1

	# Discard any staged (uncommitted) uci changes first, so they cannot be
	# replayed after the files are put back.
	uci -q revert network >/dev/null 2>&1 || true
	uci -q revert firewall >/dev/null 2>&1 || true
	uci -q revert dhcp >/dev/null 2>&1 || true
	uci -q revert kmwan >/dev/null 2>&1 || true

	msg "Restoring /etc/config/{network,firewall,dhcp} from $STATE_DIR"
	ok=1
	configs='network firewall dhcp'
	[ ! -f "$STATE_DIR/kmwan" ] || configs="$configs kmwan"
	for cfg in $configs; do
		cp "$STATE_DIR/$cfg" "/etc/config/$cfg" || {
			warn "failed to restore /etc/config/$cfg"
			ok=0
			continue
		}
		chmod 600 "/etc/config/$cfg" 2>/dev/null || true
	done
	[ "$ok" = 1 ] || return 1

	if command -v cmp >/dev/null 2>&1; then
		for cfg in $configs; do
			cmp -s "$STATE_DIR/$cfg" "/etc/config/$cfg" || {
				warn "/etc/config/$cfg does not match the snapshot"
				ok=0
			}
		done
	fi
	[ "$ok" = 1 ] || return 1

	reload_services || return 1
	restore_managed_files || return 1

	# wg0 should disappear with the restored network config, unless it was
	# part of the original configuration.
	if [ "$had_wg0" = 0 ]; then
		i=0
		while [ "$i" -lt 10 ] &&
			ip link show "$WG_IF" >/dev/null 2>&1; do
			sleep 1
			i=$((i + 1))
		done
		if ip link show "$WG_IF" >/dev/null 2>&1; then
			msg "Removing leftover $WG_IF device"
			ip link del dev "$WG_IF" >/dev/null 2>&1 || true
		fi
		if ip link show "$WG_IF" >/dev/null 2>&1; then
			warn "$WG_IF still exists after the restore"
			return 1
		fi
	fi

	return 0
}

# ---------------------------------------------------------------------------
# Targeted cleanup when no snapshot exists
# ---------------------------------------------------------------------------

cleanup_without_snapshot() {
	msg "No usable snapshot; removing the CellBridge configuration only"

	uci -q revert network >/dev/null 2>&1 || true
	uci -q revert firewall >/dev/null 2>&1 || true
	uci -q revert dhcp >/dev/null 2>&1 || true

	# network: peers first (they are sections of type wireguard_wg0), then the
	# interface itself.
	while uci -q delete "network.@wireguard_wg0[0]" >/dev/null 2>&1; do
		:
	done
	if [ -n "$(uci -q get "network.$WG_IF")" ]; then
		uci -q delete "network.$WG_IF"
		msg "Removed network.$WG_IF"
	fi

	for w in $WAN_SECTIONS; do
		name=$(section_name "network.$w")
		if [ -n "$(uci -q get "network.$w.disabled")" ]; then
			uci -q delete "network.$w.disabled"
			msg "Re-enabled WAN interface: $name"
		fi
	done

	uci commit network >/dev/null 2>&1 || warn "uci commit network failed"

	# firewall: remove every lan -> vpn forwarding, re-scanning after each
	# delete so anonymous-section indices cannot shift under us.
	while :; do
		found=
		for sec in $(uci -q show firewall |
			awk -F'[.=]' '/=forwarding$/ { print $2 }'); do
			if [ "$(uci -q get "firewall.$sec.src")" = lan ] &&
				[ "$(uci -q get "firewall.$sec.dest")" = vpn ]; then
				uci -q delete "firewall.$sec"
				msg "Removed forwarding lan -> vpn"
				found=1
				break
			fi
		done
		[ -n "$found" ] || break
	done

	# firewall: drop wg0 from the vpn zone; delete the zone only if wg0 is
	# its sole network (i.e. it looks like the one setup created).
	while :; do
		found=
		for sec in $(uci -q show firewall |
			awk -F'[.=]' '/=zone$/ { print $2 }'); do
			[ "$(uci -q get "firewall.$sec.name")" = vpn ] || continue
			has_wg0=0
			count=0
			for n in $(uci -q get "firewall.$sec.network"); do
				count=$((count + 1))
				[ "$n" = "$WG_IF" ] && has_wg0=1
			done
			[ "$has_wg0" = 1 ] || continue
			if [ "$count" -le 1 ]; then
				uci -q delete "firewall.$sec"
				msg "Removed firewall zone 'vpn'"
			else
				uci -q del_list "firewall.$sec.network=$WG_IF"
				msg "Removed $WG_IF from the existing 'vpn' zone"
			fi
			found=1
			break
		done
		[ -n "$found" ] || break
	done

	uci commit firewall >/dev/null 2>&1 || warn "uci commit firewall failed"

	# dhcp: remove the public resolver(s) setup added.  Only the default is
	# known when the snapshot is gone (setup always writes one, so this path
	# normally only follows a successful snapshot restore).
	removed_dns=0
	for d in $(uci -q get 'dhcp.@dnsmasq[0].server'); do
		if [ "$d" = "$DEFAULT_DNS" ]; then
			uci -q del_list "dhcp.@dnsmasq[0].server=$d"
			removed_dns=1
			msg "Removed dnsmasq upstream $d"
		fi
	done
	if [ "$removed_dns" = 1 ] &&
		[ -z "$(uci -q get 'dhcp.@dnsmasq[0].server')" ] &&
		[ "$(uci -q get 'dhcp.@dnsmasq[0].noresolv')" = 1 ]; then
		uci -q delete 'dhcp.@dnsmasq[0].noresolv'
		msg "Removed dnsmasq noresolv override"
	fi
	uci commit dhcp >/dev/null 2>&1 || warn "uci commit dhcp failed"

	reload_services || return 1

	# Verify that the tunnel config is gone.
	if [ -n "$(uci -q get "network.$WG_IF")" ]; then
		warn "network.$WG_IF is still configured"
		return 1
	fi
	if ip link show "$WG_IF" >/dev/null 2>&1; then
		msg "Removing leftover $WG_IF device"
		ip link del dev "$WG_IF" >/dev/null 2>&1 || true
	fi
	if ip link show "$WG_IF" >/dev/null 2>&1; then
		warn "$WG_IF still exists after cleanup"
		return 1
	fi
	return 0
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

msg "== CellBridge revert =="

if [ ! -d "$STATE_DIR" ] && [ ! -e "$INSTALL_MARKER" ] && [ -f "$RESTORED_MARKER" ]; then
	msg "This installation was already reverted; leaving the restored original state untouched."
	exit 0
fi
if snapshot_is_usable; then
	msg "Found snapshot in $STATE_DIR"
	stop_remove_failover || die "cannot stop/remove the monitor; keeping backups"
	if restore_snapshot; then
		msg "Restored the previous network, firewall, DNS and kmwan configuration."
		printf '2\n' > "$RESTORED_MARKER.new" && mv "$RESTORED_MARKER.new" "$RESTORED_MARKER" ||
			die "cannot record successful restore; keeping backups"
		rm -f "$INSTALL_MARKER" || die "cannot clear installation marker; keeping backups"
		if [ -d "$STATE_DIR" ]; then
			rm -rf "$STATE_DIR" || warn "could not remove $STATE_DIR"
		fi
		if [ -d "$STATE_DIR" ]; then
			warn "the snapshot was restored but $STATE_DIR could not be removed"
			msg "It is safe to delete $STATE_DIR manually."
		else
			msg "Removed $STATE_DIR"
		fi
	else
		warn "restore did not complete; KEEPING $STATE_DIR for a retry"
		print_wan_state
		exit 1
	fi
elif [ -d "$STATE_DIR" ]; then
	die "snapshot is incomplete or damaged; keeping it and refusing a guessed restore"
elif has_artifacts; then
	if [ -d "$STATE_DIR" ]; then
		warn "$STATE_DIR exists but is incomplete; keeping it untouched"
	fi
	stop_remove_failover || die "cannot stop/remove the monitor; keeping backups"
	if cleanup_without_snapshot; then
		msg "Removed the CellBridge configuration."
	else
		warn "cleanup did not complete; inspect the output above"
		print_wan_state
		exit 1
	fi
else
	msg "No CellBridge configuration and no snapshot found; nothing to do."
fi

print_wan_state
msg ""
msg "If the WAN does not come back within ~30s, check the cable/uplink and:"
msg "  ifup wan        # or the interface name printed above"
msg "  logread | tail -n 40"
