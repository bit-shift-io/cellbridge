#!/bin/sh
# CellBridge-managed tunnel health routing. POSIX sh / BusyBox ash.
# kmwan remains responsible for USB/WAN health and backup selection.
set -u

WG_IF=wg0
WG_SOURCE=10.10.10.1
ACTIVE_TABLE=51820
PROBE_TABLE=51821
RUN_DIR=${CELLBRIDGE_RUN_DIR:-/var/run/cellbridge-failover}
HEALTH_URLS='https://1.1.1.1/cdn-cgi/trace https://1.0.0.1/cdn-cgi/trace'

log() { logger -t cellbridge-failover "$*"; }

native_kmwan_ready() {
	# A registered cell may be offline: no working backup is a valid state.
	# What matters here is that the native monitor registered every enabled,
	# currently up IPv4 member, not that it found internet on that member.
	[ -r /proc/gl-kmwan/config ] || return 1
	native_cells=$(cat /proc/gl-kmwan/config) || return 1
	printf '%s\n' "$native_cells" | awk '
		NF && $0 !~ /^[A-Za-z0-9_.-]+:(online|offline)$/ { exit 1 }
	' || return 1
	native_config=$(uci -q show kmwan) || return 1
	for member in $(printf '%s\n' "$native_config" | awk -F'[.=]' '/=member$/ { print $2 }'); do
		[ "$(uci -q get "kmwan.$member.disabled")" != 1 ] || continue
		[ "$(uci -q get "kmwan.$member.addr_type")" = 4 ] || continue
		native_iface=$(uci -q get "kmwan.$member.interface") || return 1
		[ -n "$native_iface" ] || return 1
		# A configured USB/modem interface need not exist while disconnected.
		if native_status=$(ifstatus "$native_iface" 2>/dev/null); then
			native_up=$(printf '%s\n' "$native_status" | jsonfilter -e '@.up') || return 1
			case "$native_up" in
				false) continue ;;
				true) : ;;
				*) return 1 ;;
			esac
		else
			# Missing dynamic modem members are normal. Failure to inspect a
			# configured, enabled interface is not evidence it is disconnected.
			if [ -n "$(uci -q get "network.$native_iface")" ] &&
				[ "$(uci -q get "network.$native_iface.disabled")" != 1 ]; then
				return 1
			fi
			continue
		fi
		printf '%s\n' "$native_cells" | awk -F: -v iface="$native_iface" '
			$1 == iface && ($2 == "online" || $2 == "offline") { found=1 }
			END { exit !found }
		' || return 1
	done
}

restart_native_kmwan() {
	if /etc/init.d/kmwan restart; then
		return 0
	else
		native_rc=$?
	fi
	# Some vendor gen_weight_route versions end failover mode with:
	#   [ "$ipv6_enable" = 1 ] && add_static_rt6
	#   exit
	# With IPv6 disabled, this returns 1 after successful IPv4 initialization.
	# Bound the exception to that mode/code and verify actual registration.
	[ "$native_rc" = 1 ] &&
		[ "$(uci -q get kmwan.global.enable)" = 1 ] &&
		[ "$(uci -q get kmwan.global.mode)" = failover ] &&
		[ "$(uci -q get glipv6.globals.enabled)" = 0 ] || return "$native_rc"
	command -v ifstatus >/dev/null 2>&1 && command -v jsonfilter >/dev/null 2>&1 || return "$native_rc"
	native_attempt=0
	while [ "$native_attempt" -lt 3 ]; do
		if native_kmwan_ready; then
			printf 'warning: kmwan returned 1 in IPv4 failover mode; runtime registration verified\n' >&2
			return 0
		fi
		native_attempt=$((native_attempt + 1))
		[ "$native_attempt" -ge 3 ] || sleep 1
	done
	return "$native_rc"
}

check_ownership() {
	# Inspect all tables in one successful dump: a failed per-table lookup
	# must not be confused with an empty table. -N avoids configured aliases.
	routes=$(ip -4 -N route show table all) || return 1
	rules=$(ip -4 -N -details rule show) || return 1
	printf '%s\n' "$routes" | awk '
	/table (51820|51821)( |$)/ {
		gsub(/[[:space:]]+/, " "); sub(/ $/, "")
		table = ($0 ~ /table 51820( |$)/) ? "51820" : "51821"
		if ($0 !~ / proto 186( |$)/) exit 1
		sub(" table " table, ""); sub(/ proto 186/, "")
		sub(/ scope 253/, ""); sub(/ linkdown$/, "")
		sub(/^7 /, "unreachable ")
		if ($0 == "default dev wg0 src 10.10.10.1 metric 10") next
		if (table == "51821" && $0 == "unreachable default metric 32767") next
		exit 1
	}' || return 1
	printf '%s\n' "$rules" | awk '
	/^(100|101):|lookup (51820|51821)( |$)|from 10\.10\.10\.1( |\/)/ {
		gsub(/[[:space:]]+/, " "); sub(/ $/, "")
		if ($0 !~ / proto 186( |$)/) exit 1
		sub(/ proto 186/, "")
		if ($0 == "0: from 10.10.10.1 lookup 51821") next
		if ($0 == "100: from all lookup 254 suppress_prefixlength 0") next
		if ($0 == "101: from all lookup 51820") next
		exit 1
	}'
}

cleanup() {
	check_ownership || { printf 'Cannot inspect routing or foreign entries occupy CellBridge tables/rules\n' >&2; return 1; }
	# Delete exact rules only, never a foreign rule sharing a priority.
	while ip -4 rule del priority 0 from "$WG_SOURCE/32" lookup "$PROBE_TABLE" protocol 186 2>/dev/null; do :; done
	while ip -4 rule del priority 100 lookup main suppress_prefixlength 0 protocol 186 2>/dev/null; do :; done
	while ip -4 rule del priority 101 lookup "$ACTIVE_TABLE" protocol 186 2>/dev/null; do :; done
	ip -4 route flush table "$ACTIVE_TABLE" proto 186 2>/dev/null || true
	ip -4 route flush table "$PROBE_TABLE" proto 186 2>/dev/null || true
	rm -f "$RUN_DIR/status"
}

prepare_probe_routes() {
	# The guard prevents a failed tunnel lookup falling through to USB/WAN.
	ip -4 route replace unreachable default metric 32767 proto 186 table "$PROBE_TABLE" || return 1
	ip -4 route replace default dev "$WG_IF" src "$WG_SOURCE" metric 10 proto 186 table "$PROBE_TABLE"
}

tunnel_internet_works() {
	prepare_probe_routes >/dev/null 2>&1 || return 1
	for url in $HEALTH_URLS; do
		# Numeric HTTPS targets avoid DNS dependence; verify certificates and
		# require 200 (not a captive-portal redirect). Never use a proxy.
		code=$(curl --ipv4 --noproxy '*' --interface "$WG_IF" \
			--silent --fail --connect-timeout 2 --max-time 3 \
			--output /dev/null --write-out '%{http_code}' "$url") || continue
		[ "$code" = 200 ] && return 0
	done
	return 1
}

run_monitor() {
	umask 077
	mkdir -p "$RUN_DIR" || return 1
	# procd owns one instance. On restart, never trust an old healthy route.
	cleanup || return 1
	trap 'exit 0' INT TERM
	trap cleanup 0
	# Priority 0 follows the kernel's local rule but precedes vendor local-
	# output policy rules. Only the dedicated tunnel source uses this rule.
	ip -4 rule add priority 0 from "$WG_SOURCE/32" lookup "$PROBE_TABLE" protocol 186 || return 1
	ip -4 rule add priority 100 lookup main suppress_prefixlength 0 protocol 186 || return 1
	ip -4 rule add priority 101 lookup "$ACTIVE_TABLE" protocol 186 || return 1
	good=0
	bad=0
	active=0
	log 'Started; using backup routing until tunnel internet is verified'
	while :; do
		if tunnel_internet_works; then
			bad=0
			[ "$good" -ge 2 ] || good=$((good + 1))
			if [ "$good" -ge 2 ]; then
				# Reconcile even while active: netifd may have recreated wg0.
				if ip -4 route replace default dev "$WG_IF" src "$WG_SOURCE" metric 10 proto 186 table "$ACTIVE_TABLE"; then
					[ "$active" = 1 ] || log 'Tunnel internet recovered; preferring CellBridge'
					active=1
				else
					log 'Cannot install tunnel route; using backup routing'
					ip -4 route flush table "$ACTIVE_TABLE" proto 186 || return 1
					active=0
				fi
			fi
		else
			good=0
			[ "$bad" -ge 3 ] || bad=$((bad + 1))
			if [ "$bad" -ge 3 ] && [ "$active" = 1 ]; then
				ip -4 route flush table "$ACTIVE_TABLE" proto 186 || return 1
				active=0
				log 'Tunnel internet failed; using USB/WAN backup routing'
			fi
		fi
		path=backup
		[ "$active" = 0 ] || path=tunnel
		printf 'path=%s\nsuccesses=%s\nfailures=%s\n' "$path" "$good" "$bad" > "$RUN_DIR/status.new"
		mv "$RUN_DIR/status.new" "$RUN_DIR/status"
		sleep 2
	done
}

case "${1:-run}" in
	run) run_monitor ;;
	cleanup) cleanup ;;
	check) check_ownership ;;
	status) cat "$RUN_DIR/status" ;;
	restart-kmwan) restart_native_kmwan ;;
	*) printf 'Usage: %s [run|cleanup|check|status|restart-kmwan]\n' "$0" >&2; exit 1 ;;
esac
