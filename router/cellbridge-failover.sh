#!/bin/sh
# CellBridge-managed tunnel health routing. POSIX sh / BusyBox ash.
# kmwan remains responsible for USB/WAN health and backup selection.
set -u

ACTIVE_TABLE=51820
PHONES_FILE=${CELLBRIDGE_PHONES_FILE:-/etc/cellbridge-phones.conf}
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

load_phones() {
	if [ ! -e "$PHONES_FILE" ]; then
		printf 'wg0 10.10.10.1 51820 51821 10 -\n'
		return 0
	fi
	[ -f "$PHONES_FILE" ] && [ -r "$PHONES_FILE" ] || return 1
	# Validate completely before publishing any records. Never evaluate as shell.
	awk '
	/^[[:space:]]*(#|$)/ { next }
	{
		if (NF != 6 || $1 !~ /^wg[0-7]$/) exit 1
		n = substr($1, 3) + 0
		if ($2 != "10.10." (10+n) ".1" || $4 != 51821+10*n) exit 1
		if ($3 !~ /^[0-9]+$/ || $3+0 < 1 || $3+0 > 65535) exit 1
		if ($5 !~ /^[0-9]+$/ || $5+0 < 1 || $5+0 > 2147483647) exit 1
		if ($6 !~ /^[A-Za-z0-9_.-]+$/ || length($6) > 64) exit 1
		if (iface[$1]++ || port[$3+0]++ || priority[$5+0]++) exit 1
		rows[++count] = $1 " " $2 " " ($3+0) " " ($4+0) " " ($5+0) " " $6
	}
	END {
		if (!count) exit 1
		# An earlier exit must not leak partial configuration to callers.
		for (i=1; i<=count; i++) print rows[i]
	}' "$PHONES_FILE"
}

check_ownership() {
	# Inspect all tables in one successful dump: a failed per-table lookup
	# must not be confused with an empty table. -N avoids configured aliases.
	routes=$(ip -4 -N route show table all) || return 1
	rules=$(ip -4 -N -details rule show) || return 1
	printf '%s\n' "$routes" | awk '
	{
		table = ""
		for (i=1; i<NF; i++) if ($i == "table") table = $(i+1)
		if (table+0 < 51820 || table+0 > 51891) next
		gsub(/[[:space:]]+/, " "); sub(/ $/, "")
		if ($0 !~ / proto 186( |$)/) exit 1
		sub(" table " table, ""); sub(/ proto 186/, "")
		sub(/ scope 253/, ""); sub(/ linkdown$/, "")
		sub(/^7 /, "unreachable ")
		valid = 0
		for (n=0; n<8; n++) {
			probe = 51821+10*n
			if (table != 51820 && table != probe) continue
			if ($0 == "default dev wg" n " src 10.10." (10+n) ".1 metric 10") valid=1
			if (table == probe && $0 == "unreachable default metric 32767") valid=1
		}
		if (valid) next
		exit 1
	}' || return 1
	printf '%s\n' "$rules" | awk '
	{
		relevant = ($0 ~ /^(100|101):/)
		for (i=1; i<NF; i++) if ($i == "lookup" && $(i+1)+0 >= 51820 && $(i+1)+0 <= 51891) relevant=1
		for (n=0; n<8; n++) if (index($0, "from 10.10." (10+n) ".1 ") || index($0, "from 10.10." (10+n) ".1/")) relevant=1
		if (!relevant) next
		gsub(/[[:space:]]+/, " "); sub(/ $/, "")
		if ($0 !~ / proto 186( |$)/) exit 1
		sub(/ proto 186/, "")
		sub(/\/32 /, " ")
		valid=0
		for (n=0; n<8; n++) if ($0 == "0: from 10.10." (10+n) ".1 lookup " (51821+10*n)) valid=1
		if (valid) next
		if ($0 == "100: from all lookup 254 suppress_prefixlength 0") next
		if ($0 == "101: from all lookup 51820") next
		exit 1
	}'
}

flush_owned_table() {
	ip -4 route flush table "$1" proto 186 2>/dev/null && return 0
	# Linux returns an error for a table never created. Confirm absence via a
	# successful full dump; never confuse a failed inspection with an empty FIB.
	remaining=$(ip -4 -N route show table all) || return 1
	printf '%s\n' "$remaining" | awk -v table="$1" '
		{for(i=1;i<NF;i++) if($i=="table" && $(i+1)==table) exit 1}'
}

cleanup() {
	check_ownership || { printf 'Cannot inspect routing or foreign entries occupy CellBridge tables/rules\n' >&2; return 1; }
	# Delete exact rules only, never a foreign rule sharing a priority.
	n=0
	while [ "$n" -lt 8 ]; do
		while ip -4 rule del priority 0 from "10.10.$((10+n)).1/32" lookup "$((51821+10*n))" protocol 186 2>/dev/null; do :; done
		flush_owned_table "$((51821+10*n))" || return 1
		n=$((n+1))
	done
	while ip -4 rule del priority 100 lookup main suppress_prefixlength 0 protocol 186 2>/dev/null; do :; done
	while ip -4 rule del priority 101 lookup "$ACTIVE_TABLE" protocol 186 2>/dev/null; do :; done
	flush_owned_table "$ACTIVE_TABLE" || return 1
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

read_state() {
	good=0 bad=0 health=unverified
	[ ! -f "$RUN_DIR/$iface.health" ] || read -r good bad health < "$RUN_DIR/$iface.health"
}

stop_monitor() {
	trap - 0 INT TERM
	[ -z "$children" ] || kill $children 2>/dev/null || true
	[ -z "$children" ] || wait $children 2>/dev/null || true
	cleanup
	rm -f "$RUN_DIR"/wg[0-7].health "$RUN_DIR"/wg[0-7].result "$RUN_DIR"/wg[0-7].wifi
}

next_candidate() {
	# Move past a failed candidate, wrapping so cellular recovery is retried.
	candidate=$(printf '%s\n' "$phones" | awk -v current="$1" '
		first == "" { first=$1 } found { print $1; exit } $1 == current { found=1 }
		END { if (!found || $1 == current) print first }')
}

run_monitor() {
	umask 077
	mkdir -p "$RUN_DIR" || return 1
	# procd owns one instance. On restart, never trust an old healthy route.
	cleanup || return 1
	phones=$(load_phones) || { log 'Invalid phone configuration; using backup routing'; return 1; }
	phones=$(printf '%s\n' "$phones" | sort -k5,5n)
	children=
	trap 'exit 0' INT TERM
	trap stop_monitor 0
	# Priority 0 follows the kernel's local rule but precedes vendor local-
	# output policy rules. Only the dedicated tunnel source uses this rule.
	while read -r iface source port table priority label; do
		printf '0 0 unverified\n' > "$RUN_DIR/$iface.health"
		ip -4 rule add priority 0 from "$source/32" lookup "$table" protocol 186 || return 1
	done <<EOF
$phones
EOF
	ip -4 rule add priority 100 lookup main suppress_prefixlength 0 protocol 186 || return 1
	ip -4 rule add priority 101 lookup "$ACTIVE_TABLE" protocol 186 || return 1
	selected=
	selected_priority=2147483648
	candidate=$(printf '%s\n' "$phones" | awk 'NR==1 {print $1}')
	last_wifi=0
	log 'Started; using backup routing until tunnel internet is verified'
	while :; do
		now=$(date +%s) || return 1
		children=
		while read -r iface source port table priority label; do
			if [ $((now-last_wifi)) -ge 25 ]; then
				# Passive observation only: a sent keepalive is not proof of life.
				handshake=$(wg show "$iface" latest-handshakes 2>/dev/null | awk 'NR==1 {print $2}')
				transfer=$(wg show "$iface" transfer 2>/dev/null | awk 'NR==1 {print $2}')
				printf 'last_handshake=%s received_bytes=%s\n' "${handshake:-unknown}" "${transfer:-unknown}" > "$RUN_DIR/$iface.wifi"
			fi
			if [ -n "$selected" ]; then
				[ "$priority" -le "$selected_priority" ] || continue
			else
				[ "$iface" = "$candidate" ] || continue
			fi
			(
				WG_IF=$iface WG_SOURCE=$source PROBE_TABLE=$table
				if tunnel_internet_works; then printf 'good\n'; else printf 'bad\n'; fi > "$RUN_DIR/$iface.result"
			) &
			children="$children $!"
		done <<EOF
$phones
EOF
		for child in $children; do wait "$child" || return 1; done
		children=
		[ $((now-last_wifi)) -lt 25 ] || last_wifi=$now
		old_selected=$selected
		choice= choice_source= choice_priority=2147483648
		while read -r iface source port table priority label; do
			read_state
			if [ -f "$RUN_DIR/$iface.result" ]; then
				read -r result < "$RUN_DIR/$iface.result"
				rm -f "$RUN_DIR/$iface.result"
				if [ "$result" = good ]; then
					bad=0; [ "$good" -ge 2 ] || good=$((good+1))
					[ "$good" -lt 2 ] || health=healthy
				else
					good=0; [ "$bad" -ge 3 ] || bad=$((bad+1))
					if [ "$iface" != "$selected" ] || [ "$bad" -ge 3 ]; then health=unhealthy; fi
					if [ -z "$selected" ] && [ "$iface" = "$candidate" ]; then next_candidate "$candidate"; fi
				fi
				printf '%s %s %s\n' "$good" "$bad" "$health" > "$RUN_DIR/$iface.health"
			fi
			if [ -z "$choice" ] && [ "$health" = healthy ]; then
				choice=$iface choice_source=$source choice_priority=$priority
			fi
		done <<EOF
$phones
EOF
		if [ -n "$choice" ] && ip -4 route replace default dev "$choice" src "$choice_source" metric 10 proto 186 table "$ACTIVE_TABLE"; then
			selected=$choice selected_priority=$choice_priority
		else
			flush_owned_table "$ACTIVE_TABLE" || return 1
			selected= selected_priority=2147483648
			if [ -n "$choice" ]; then
				# A vanished device cannot retain healthy status and permanently
				# outrank freshly verified fallback candidates.
				printf '0 0 unhealthy\n' > "$RUN_DIR/$choice.health"
				next_candidate "$choice"
			elif [ -n "$old_selected" ]; then
				next_candidate "$old_selected"
			fi
		fi
		if [ "$selected" != "$old_selected" ]; then
			log "Selected ${selected:-USB/WAN backup routing}"
			# Lower-preference phones must freshly qualify if needed later.
			while read -r iface source port table priority label; do
				[ "$priority" -le "$selected_priority" ] || printf '0 0 unverified\n' > "$RUN_DIR/$iface.health"
			done <<EOF
$phones
EOF
		fi
		path=backup
		good=0 bad=0
		if [ -n "$selected" ]; then path=tunnel; iface=$selected; read_state; fi
		{
			printf 'path=%s\nselected=%s\nsuccesses=%s\nfailures=%s\n' "$path" "${selected:-none}" "$good" "$bad"
			while read -r iface source port table priority label; do
				read_state
				printf '%s priority=%s label=%s health=%s successes=%s failures=%s ' "$iface" "$priority" "$label" "$health" "$good" "$bad"
				cat "$RUN_DIR/$iface.wifi" 2>/dev/null || printf 'last_handshake=unknown received_bytes=unknown\n'
			done <<EOF
$phones
EOF
		} > "$RUN_DIR/status.new"
		mv "$RUN_DIR/status.new" "$RUN_DIR/status"
		sleep 2
	done
}

case "${1:-run}" in
	run) run_monitor ;;
	cleanup) cleanup ;;
	check) check_ownership ;;
	phones) phones=$(load_phones) || { printf 'Invalid phone configuration\n' >&2; exit 1; }; printf '%s\n' "$phones" ;;
	status) cat "$RUN_DIR/status" ;;
	restart-kmwan) restart_native_kmwan ;;
	*) printf 'Usage: %s [run|cleanup|check|phones|status|restart-kmwan]\n' "$0" >&2; exit 1 ;;
esac
