#!/bin/sh
# CellBridge router setup for the GL.iNet Flint 3 (GL-BE9300), OpenWrt + fw4.
#
# Topology:
#
#   [LAN client] --> [Flint 3: default route via wg0, masquerade]
#                          |
#                          v  WireGuard over Wi-Fi
#                 [Phone: boringtun -> userspace gateway -> cellular sockets]
#                          |
#                          v
#                      [Internet]
#
# The router is the WireGuard *server* (wg0, 10.10.10.1/24, UDP 51820 by
# default; use --port if 51820 is already used by another WireGuard service).
# The phone is the only peer (10.10.10.2/24) and the *initiator*: it is a Wi-Fi
# client of this router, so no static DHCP lease or fixed LAN address is
# needed.  The router sends the LAN's default traffic into the tunnel and
# masquerades it, so the phone sees every LAN client as 10.10.10.1.
#
# Prefer verified tunnel internet, then USB tethering, then WAN. kmwan keeps
# managing physical uplinks; the companion health monitor adds tunnel routing.
# All missing dependencies are installed before changing routing.
#
# Route note -- do not "fix" this:
#   `route_allowed_ips 0` leaves default routing to the health monitor.
#   Connected main-table routes are consulted before its preferred tunnel
#   table, so the phone's LAN endpoint stays reachable through br-lan.
#   Do not replace the LAN route with a broader one and do not route the
#   phone's LAN address into wg0.
#
# The same `allowed_ips 0.0.0.0/0` is also the inbound source filter: replies
# from the internet arrive from arbitrary addresses and must be accepted from
# the phone.  Narrowing it would silently drop those replies.
#
# Usage:
#   sh cellbridge-setup.sh
#
# Options:
#   --phone-public-key KEY   use a phone-managed key instead of generating one
#   --dns ADDR               upstream resolver for dnsmasq (repeatable,
#                            default 1.1.1.1)
#   --wan-iface NAME         extra backup UCI interface (repeatable);
#                            normally auto-detected
#   --port PORT              WireGuard UDP port (default 51820; re-runs reuse
#                            the port already configured on wg0)
#   --keep-wan               accepted for compatibility; backups now stay on
#   -h, --help               this text
#
# By default a generated phone key pair is stored as owner-only peer metadata
# and included in the QR. Re-runs reuse it. --phone-public-key retains the
# original phone-managed-key flow without importing a phone private key.
#
# The script generates (or reuses) the peer's preshared key and prints the
# phone's settings plus a QR code for the CellBridge app.  It tries to install
# `qrencode` while the WAN is still up; if that fails, the text settings and
# the raw payload are printed instead.
#
# POSIX sh / BusyBox ash only.  No bashisms.
#
# Run as root on the router.  The script is idempotent and may be re-run on a
# partially configured router.

set -u

STATE_DIR=/etc/cellbridge-backup
WG_IF=wg0
WG_ADDR=10.10.10.1/24
WG_ADDR_HOST=10.10.10.1
WG_PORT=51820
WG_PORT_EXPLICIT=0
WG_MTU=1420
PHONE_ADDR=10.10.10.2/24
DEFAULT_DNS=1.1.1.1
LAN_DEV=br-lan
SCRIPT_DIR=$(CDPATH= cd "$(dirname "$0")" && pwd) || exit 1
MONITOR_SOURCE=$SCRIPT_DIR/cellbridge-failover.sh
MONITOR=/usr/sbin/cellbridge-failover
MONITOR_INIT=/etc/init.d/cellbridge-failover
INSTALL_MARKER=/etc/cellbridge-installed
RESTORED_MARKER=/etc/cellbridge-restored
PHONES_FILE=/etc/cellbridge-phones.conf
ADD_PHONE=0
PHONE_EXPLICIT=0
LIST_PHONES=0
PRIORITY=
LABEL=
METADATA_ONLY=0
NETWORK_OPTIONS=0
DNS_EXPLICIT=0
REGISTER=0
APP_OUTPUT=0

# In app mode every diagnostic goes to stderr; the sole machine-readable
# setup payload is the only thing written to stdout, so the phone can parse
# stdout without mistaking progress text for a result.
msg()  {
	if [ "$APP_OUTPUT" = 1 ]; then
		printf '%s\n' "$*" >&2
	else
		printf '%s\n' "$*"
	fi
}
warn() { printf 'warning: %s\n' "$*" >&2; }
die()  { printf 'error: %s\n' "$*" >&2; exit 1; }

is_wireguard_key() {
	[ "${#1}" -eq 44 ] || return 1
	case "$1" in
	*=) : ;;
	*) return 1 ;;
	esac
	case "${1%?}" in
	*[!A-Za-z0-9+/]*) return 1 ;;
	esac
	return 0
}

wg0_link_up() {
	ip link show "$WG_IF" 2>/dev/null |
		head -n 1 |
		sed -n 's/.*<\([^>]*\)>.*/\1/p' |
		tr ',' '\n' |
		grep -qx UP
}

udp_port_listening() {
	_port=$1
	_hex=$(printf '%04X' "$_port" 2>/dev/null) || return 1
	for _table in /proc/net/udp /proc/net/udp6; do
		[ -r "$_table" ] || continue
		awk -v hex="$_hex" '
			NR > 1 {
				n = split($2, parts, ":")
				if (n > 1 && toupper(parts[n]) == hex) { found = 1; exit }
			}
			END { exit !found }
		' "$_table" && return 0
	done
	return 1
}

wg0_owns_udp_port() {
	wg0_link_up &&
		[ "$(wg show "$WG_IF" listen-port 2>/dev/null)" = "$WG_PORT" ]
}

other_wg_port_owners() {
	command -v wg >/dev/null 2>&1 || return 0
	for _iface in $(wg show interfaces 2>/dev/null); do
		[ "$_iface" = "$WG_IF" ] && continue
		[ "$(wg show "$_iface" listen-port 2>/dev/null)" = "$WG_PORT" ] &&
			printf '%s\n' "$_iface"
	done
}

check_wg_port_available() {
	udp_port_listening "$WG_PORT" || return 0
	wg0_owns_udp_port && return 0
	_owners=$(other_wg_port_owners)
	if [ -n "$_owners" ]; then
		_owner_list=$(printf '%s' "$_owners" | tr '\n' ' ')
		die "UDP $WG_PORT is already used by WireGuard interface(s): ${_owner_list% }; rerun with --port PORT"
	fi
	die "UDP $WG_PORT is already in use; rerun with --port PORT"
}

usage() {
	cat <<'EOF'
CellBridge router setup (GL.iNet Flint 3 / OpenWrt fw4)

Usage:
  sh cellbridge-setup.sh [options]

Options:
  --add-phone              add the lowest free phone index (up to 8 phones)
  --phone wgN              target an existing phone (default wg0)
  --priority N             unique preference; lower is preferred
  --label NAME             display token (letters, digits, dot, dash, underscore)
  --list-phones            read-only list of configured phones
  --register               app mode: reuse the phone whose public key matches,
                           otherwise add the next free phone (use with
                           --phone-public-key and --priority)
  --app-output             write diagnostics to stderr and only the machine
                           setup payload to stdout
  --phone-public-key KEY   optional phone-managed public key (skip generation)
  --dns ADDR               upstream resolver (repeatable, default 1.1.1.1)
  --wan-iface NAME         extra backup UCI interface (repeatable)
  --port PORT              WireGuard UDP port (default 51820)
  --keep-wan               compatibility option; backups now always stay on
  -h, --help               show this help

Re-runs reuse the port already configured on wg0 unless --port is given. If the
chosen port is occupied, setup stops before changing anything and tells you to
choose another with --port.

Default: generate/reuse phone keys and import them by scanning the QR. The app
asks Yes/No before overwriting different existing phone keys. Save applies them.
Generated keys are stored in root-only network peer metadata for later re-runs.
Treat the QR and any printed raw QR payload as private key material.

It prints the phone's WireGuard settings, including the generated or reused
preshared key, plus a QR code for the CellBridge app.  Setup installs
`qrencode` while the WAN is up; if that fails, the text settings and the raw
payload are printed instead.

Copy cellbridge-failover.sh beside this script before running it. Requires
GL.iNet kmwan enabled. Priority: verified CellBridge internet, USB tether, WAN.
EOF
}

# ---------------------------------------------------------------------------
# Arguments
# ---------------------------------------------------------------------------

PHONE_PUB=${CELLBRIDGE_PHONE_PUBLIC_KEY:-}
PHONE_PRIV=
MANUAL_PHONE_KEY=0
[ -z "$PHONE_PUB" ] || MANUAL_PHONE_KEY=1
DNS_LIST=
WAN_OVERRIDES=

while [ $# -gt 0 ]; do
	case "$1" in
	--add-phone) ADD_PHONE=1; shift ;;
	--register) REGISTER=1; shift ;;
	--app-output) APP_OUTPUT=1; shift ;;
	--list-phones) LIST_PHONES=1; shift ;;
	--phone|--priority|--label)
		[ $# -ge 2 ] || die "$1 needs a value"
		[ -n "$2" ] || die "$1 needs a nonempty value"
		case "$1" in
		--phone) WG_IF=$2; PHONE_EXPLICIT=1 ;;
		--priority) PRIORITY=$2; METADATA_ONLY=1 ;;
		--label) LABEL=$2; METADATA_ONLY=1 ;;
		esac
		shift 2 ;;
	--phone=*|--priority=*|--label=*)
		[ -n "${1#*=}" ] || die "${1%%=*} needs a nonempty value"
		case "$1" in
		--phone=*) WG_IF=${1#*=}; PHONE_EXPLICIT=1 ;;
		--priority=*) PRIORITY=${1#*=}; METADATA_ONLY=1 ;;
		--label=*) LABEL=${1#*=}; METADATA_ONLY=1 ;;
		esac
		shift ;;
	--phone-public-key)
		NETWORK_OPTIONS=1
		[ $# -ge 2 ] || die "--phone-public-key needs a value"
		PHONE_PUB=$2
		MANUAL_PHONE_KEY=1
		shift 2
		;;
	--phone-public-key=*)
		NETWORK_OPTIONS=1
		PHONE_PUB=${1#*=}
		MANUAL_PHONE_KEY=1
		shift
		;;
	--dns)
		NETWORK_OPTIONS=1
		DNS_EXPLICIT=1
		[ $# -ge 2 ] || die "--dns needs a value"
		DNS_LIST="$DNS_LIST $2"
		shift 2
		;;
	--dns=*)
		NETWORK_OPTIONS=1
		DNS_EXPLICIT=1
		DNS_LIST="$DNS_LIST ${1#*=}"
		shift
		;;
	--wan-iface)
		NETWORK_OPTIONS=1
		[ $# -ge 2 ] || die "--wan-iface needs a value"
		WAN_OVERRIDES="$WAN_OVERRIDES $2"
		shift 2
		;;
	--wan-iface=*)
		NETWORK_OPTIONS=1
		WAN_OVERRIDES="$WAN_OVERRIDES ${1#*=}"
		shift
		;;
	--port)
		NETWORK_OPTIONS=1
		[ $# -ge 2 ] || die "--port needs a value"
		WG_PORT=$2
		WG_PORT_EXPLICIT=1
		shift 2
		;;
	--port=*)
		NETWORK_OPTIONS=1
		WG_PORT=${1#*=}
		WG_PORT_EXPLICIT=1
		shift
		;;
	--keep-wan)
		shift
		;;
	-h|--help)
		usage
		exit 0
		;;
	*)
		usage >&2
		die "unknown argument: $1"
		;;
	esac
done

[ "$ADD_PHONE$PHONE_EXPLICIT" != 11 ] || die "--add-phone and --phone are mutually exclusive"
[ "$REGISTER" = 0 ] || [ "$ADD_PHONE" = 0 ] || die "--register and --add-phone are mutually exclusive"
[ "$REGISTER" = 0 ] || [ "$PHONE_EXPLICIT" = 0 ] || die "--register and --phone are mutually exclusive"
if [ "$REGISTER" = 1 ]; then
	[ "$MANUAL_PHONE_KEY" = 1 ] || die "--register needs --phone-public-key"
fi
[ "$APP_OUTPUT" = 0 ] || [ "$REGISTER" = 1 ] || die "--app-output is only supported with --register"
if [ "$APP_OUTPUT" = 1 ]; then
	# Reserve stdout for the final result, including output from subprocesses.
	exec 3>&1
	exec 1>&2
fi
case "$WG_IF" in wg[0-7]) : ;; *) die "--phone must be wg0 through wg7" ;; esac
if [ -n "$PRIORITY" ]; then
	case "$PRIORITY" in *[!0-9]*) die "--priority must be a positive integer" ;; esac
	[ "${#PRIORITY}" -le 10 ] && [ "$PRIORITY" -ge 1 ] && [ "$PRIORITY" -le 2147483647 ] || die "invalid --priority"
fi
if [ -n "$LABEL" ]; then
	case "$LABEL" in *[!A-Za-z0-9_.-]*) die "invalid --label" ;; esac
	[ "${#LABEL}" -le 64 ] || die "--label is too long"
fi

[ -n "$DNS_LIST" ] || DNS_LIST=$DEFAULT_DNS

case "$WG_PORT" in
	''|*[!0-9]*) die "--port must be a number between 1 and 65535" ;;
esac
[ "$WG_PORT" -ge 1 ] && [ "$WG_PORT" -le 65535 ] ||
	die "--port must be a number between 1 and 65535"

# Strip whitespace the operator may have pasted along with the key.
PHONE_PUB=$(printf '%s' "$PHONE_PUB" | tr -d '[:space:]')
if [ "$MANUAL_PHONE_KEY" = 1 ]; then
	is_wireguard_key "$PHONE_PUB" || die "the phone public key must be a 44-character base64 WireGuard key"
fi

for d in $DNS_LIST; do
	case "$d" in
		*[!A-Za-z0-9.:#-]*) die "invalid --dns value: $d" ;;
	esac
done

# ---------------------------------------------------------------------------
# Preflight
# ---------------------------------------------------------------------------

[ "$(id -u 2>/dev/null)" = 0 ] || die "this script must run as root on the router"
command -v uci >/dev/null 2>&1 || die "uci not found; is this OpenWrt?"
command -v ip  >/dev/null 2>&1 || die "ip not found"
[ -d /etc/config ] || die "/etc/config not found; is this OpenWrt?"
[ ! -e /etc/cellbridge-cleanup ] || die "a previous cleanup is incomplete; finish revert before setup"

uci -q get 'dhcp.@dnsmasq[0]' >/dev/null 2>&1 ||
	die "no dnsmasq section in /etc/config/dhcp; refusing to guess"
[ -f "$MONITOR_SOURCE" ] || die "copy cellbridge-failover.sh beside this setup script first"
sh -n "$MONITOR_SOURCE" || die "the companion monitor has invalid shell syntax"

# Interface ownership is explicit. Only a proven pre-marker wg0 may migrate.
legacy_phone() {
	[ "$1" = wg0 ] || return 1
	[ -f "$INSTALL_MARKER" ] || [ -f "$STATE_DIR/snapshot-complete" ] || return 1
	[ "$(uci -q get network.wg0.proto)" = wireguard ] || return 1
	[ "$(uci -q get network.wg0.addresses)" = 10.10.10.1/24 ] || return 1
	[ "$(uci -q show network | awk -F= '$2 == "wireguard_wg0" {n++} END {print n+0}')" = 1 ] || return 1
	[ "$(uci -q get 'network.@wireguard_wg0[0].allowed_ips')" = 0.0.0.0/0 ] &&
		[ "$(uci -q get 'network.@wireguard_wg0[0].route_allowed_ips')" = 0 ]
}

# Print the interface whose peer already uses the given public key, if any.
find_phone_by_key() {
	_key=$1
	for _i in 0 1 2 3 4 5 6 7; do
		[ -n "$(uci -q get "network.wg$_i")" ] || continue
		_pk=$(uci -q get "network.@wireguard_wg$_i[0].public_key")
		[ "$_pk" = "$_key" ] || continue
		printf 'wg%s\n' "$_i"
		return 0
	done
	return 1
}

phones=
if [ -e "$PHONES_FILE" ]; then
	phones=$(CELLBRIDGE_PHONES_FILE="$PHONES_FILE" sh "$MONITOR_SOURCE" phones) || die "invalid phone configuration; repair $PHONES_FILE"
elif [ -n "$(uci -q get network.wg0)" ]; then
	if [ "$(uci -q get network.wg0.cellbridge_managed)" = 1 ] || legacy_phone wg0; then
		legacy_port=$(uci -q get network.wg0.listen_port)
		phones="wg0 10.10.10.1 $legacy_port 51821 10 -"
	fi
fi
if [ "$LIST_PHONES" = 1 ]; then
	if [ -n "$phones" ]; then printf '%s\n' "$phones"; else msg 'No configured phones'; fi
	exit 0
fi

# Do not let stale inventory silently rotate credentials or overwrite a tunnel.
if [ -n "$phones" ]; then
	while read -r p_if p_source p_port p_table p_priority p_label; do
		[ "$(uci -q get "network.$p_if.cellbridge_managed")" = 1 ] || legacy_phone "$p_if" || die "cannot establish ownership of $p_if"
		[ "$(uci -q get "network.$p_if.proto")" = wireguard ] &&
			[ "$(uci -q get "network.$p_if.addresses")" = "$p_source/24" ] &&
			[ "$(uci -q get "network.$p_if.listen_port")" = "$p_port" ] || die "phone configuration disagrees with UCI for $p_if; repair before retrying"
	done <<EOF
$phones
EOF
fi

# App registration: reuse this phone's existing allocation when its public key
# is already configured, so a retried or repeated setup never duplicates it.
if [ "$REGISTER" = 1 ]; then
	if matching=$(find_phone_by_key "$PHONE_PUB"); then
		WG_IF=$matching
		PHONE_EXPLICIT=1
		msg "Reusing existing phone $WG_IF for the supplied public key"
	else
		# The first phone must run normal global router setup. Only subsequent
		# phones use the non-disruptive add-phone path.
		[ -z "$phones" ] || ADD_PHONE=1
		msg "Registering a new phone for the supplied public key"
	fi
fi

if [ "$ADD_PHONE" = 1 ]; then
	index=0 free_index=
	while [ "$index" -lt 8 ]; do
		if ! printf '%s\n' "$phones" | awk -v name="wg$index" '$1==name {found=1} END {exit !found}'; then
			[ -z "$(uci -q get "network.wg$index")" ] || die "stale wg$index exists in UCI; explicitly repair it before adding a phone"
			[ -n "$free_index" ] || free_index=$index
		fi
		index=$((index+1))
	done
	[ -n "$free_index" ] || die "phone limit reached (8)"
	WG_IF=wg$free_index
fi
index=${WG_IF#wg}
WG_ADDR_HOST=10.10.$((10+index)).1
WG_ADDR=$WG_ADDR_HOST/24
PHONE_ADDR=10.10.$((10+index)).2/24
PROBE_TABLE=$((51821+10*index))
[ "$WG_PORT_EXPLICIT" = 1 ] || WG_PORT=$((51820+10*index))
existing_record=$(printf '%s\n' "$phones" | awk -v name="$WG_IF" '$1==name')
if [ -n "$(uci -q get "network.$WG_IF")" ]; then
	[ "$(uci -q get "network.$WG_IF.cellbridge_managed")" = 1 ] || legacy_phone "$WG_IF" || die "refusing to adopt unrelated $WG_IF"
elif [ "$PHONE_EXPLICIT" = 1 ]; then
	die "--phone targets an existing phone; use --add-phone"
fi
if [ -n "$existing_record" ]; then
	[ -n "$PRIORITY" ] || PRIORITY=$(printf '%s\n' "$existing_record" | awk '{print $5}')
	[ -n "$LABEL" ] || LABEL=$(printf '%s\n' "$existing_record" | awk '{print $6}')
fi
[ -n "$PRIORITY" ] || PRIORITY=$((10*(index+1)))
[ -n "$LABEL" ] || LABEL=-
printf '%s\n' "$phones" | awk -v name="$WG_IF" -v priority="$PRIORITY" '$1!=name && $5==priority {exit 1}' || die "priority is already used; choose --priority"

write_phones() {
	# Publish only a fully validated file, with root-only permissions from umask.
	{
		printf '%s\n' "$phones" | awk -v name="$WG_IF" 'NF && $1!=name'
		printf '%s %s %s %s %s %s\n' "$WG_IF" "$WG_ADDR_HOST" "$WG_PORT" "$PROBE_TABLE" "$PRIORITY" "$LABEL"
	} | sort -k1,1 > "$PHONES_FILE.new" || die "cannot write phone configuration"
	CELLBRIDGE_PHONES_FILE="$PHONES_FILE.new" sh "$MONITOR_SOURCE" phones >/dev/null || die "invalid proposed phone configuration"
	chmod 600 "$PHONES_FILE.new" && mv "$PHONES_FILE.new" "$PHONES_FILE" || die "cannot publish phone configuration"
}
[ -x /etc/init.d/kmwan ] && [ "$(uci -q get kmwan.global.enable)" = 1 ] ||
	die "this failover setup requires GL.iNet kmwan enabled for physical uplinks"
for path in "$MONITOR" "$MONITOR_INIT"; do
	if [ -e "$path" ] && ! grep -q 'CellBridge-managed' "$path"; then
		die "refusing to overwrite an unrelated file: $path"
	fi
done

# Private key material must never land in a world-readable file.  The backup
# directory contains /etc/config/network, which holds the WireGuard key.
umask 077

# ---------------------------------------------------------------------------
# WAN detection (before anything is changed)
# ---------------------------------------------------------------------------

# Print the UCI *section* names of interfaces that look like WAN uplinks.
# Stock Flint 3 uses `wan` and `wan6`; `wan_6`, `wwan*`, `usbwan*` and names
# ending in `_wan` are covered too, because GL.iNet's firmware differs by
# model.  LAN, loopback and our tunnel are never candidates.
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
			wan*|wwan*|usbwan*|*_wan|secondwan*|tethering*) printf '%s\n' "$sec" ;;
		esac
	done
}

WAN_SECTIONS=$(detect_wan_sections)

# Explicit --wan-iface values are additive; they are for firmware whose WAN
# does not use a `wan*` name.  Validate them before touching anything.
for w in $WAN_OVERRIDES; do
	[ -n "$(uci -q get "network.$w")" ] ||
		die "--wan-iface '$w' is not a UCI interface section"
	case " $(printf '%s' "$WAN_SECTIONS") " in
		*" $w "*) : ;;
		*) WAN_SECTIONS="$WAN_SECTIONS $w" ;;
	esac
done

# ---------------------------------------------------------------------------
# Snapshot
# ---------------------------------------------------------------------------

# Take the snapshot exactly once and never overwrite an earlier payload.
# Incomplete/damaged new-format snapshots require explicit repair.
copy_snapshot_file() {
	[ ! -f "$2" ] || return 0
	# Interrupted/failed copies never become reusable snapshot entries.
	cp -p "$1" "$2.new" && cmp -s "$1" "$2.new" &&
		chmod 600 "$2.new" && mv "$2.new" "$2"
}

snapshot_marker() {
	printf '%s\n' "$2" > "$STATE_DIR/$1.new" &&
		mv "$STATE_DIR/$1.new" "$STATE_DIR/$1"
}

take_snapshot() {
	if [ -d "$STATE_DIR" ]; then
		[ -f "$STATE_DIR/snapshot-complete" ] ||
			die "incomplete snapshot retained; repair it before retrying setup"
		if [ -f "$STATE_DIR/snapshot-version" ]; then
			case "$(cat "$STATE_DIR/snapshot-version")" in
			2) : ;; # One-time extension of the genuine single-phone format.
			3) [ -f "$STATE_DIR/phones-file-snapshot" ] || die "completed snapshot is missing phone inventory metadata" ;;
			*) die "unsupported snapshot version" ;;
			esac
			for item in kmwan-snapshot managed-files-snapshot failover-enabled failover-running; do
				[ -f "$STATE_DIR/$item" ] || die "completed snapshot is missing metadata: $item"
			done
			[ "$(cat "$STATE_DIR/snapshot-complete")" = complete ] &&
				[ "$(cat "$STATE_DIR/managed-files-snapshot")" = complete ] || die "invalid snapshot completion metadata"
			case "$(cat "$STATE_DIR/kmwan-snapshot")" in
				present|absent) : ;;
				*) die "invalid kmwan snapshot metadata" ;;
			esac
			for item in failover-enabled failover-running; do
				grep -qx '[01]' "$STATE_DIR/$item" || die "invalid service snapshot state"
			done
		elif [ -e "$INSTALL_MARKER" ] || [ -e "$MONITOR" ] || [ -e "$MONITOR_INIT" ]; then
			die "snapshot version is missing; refusing to recapture installed files"
		fi
		if [ -f "$STATE_DIR/phones-file-snapshot" ]; then
			case "$(cat "$STATE_DIR/phones-file-snapshot")" in
			present) [ -f "$STATE_DIR/phones-config" ] || die "completed backup is missing phone inventory" ;;
			absent) : ;;
			*) die "invalid phone inventory snapshot metadata" ;;
			esac
		fi
	fi
	# A completed snapshot is immutable. Filling a lost file from today's
	# modified configuration would silently destroy the only rollback copy.
	if [ -f "$STATE_DIR/snapshot-complete" ]; then
		for cfg in network firewall dhcp; do
			[ -f "$STATE_DIR/$cfg" ] || die "completed backup is missing $cfg; refusing to replace it"
		done
		if [ -f "$STATE_DIR/kmwan-snapshot" ] &&
			[ "$(cat "$STATE_DIR/kmwan-snapshot")" = present ]; then
			[ -f "$STATE_DIR/kmwan" ] || die "completed backup is missing kmwan"
		fi
		if [ -f "$STATE_DIR/managed-files-snapshot" ]; then
			for item in failover-worker failover-init; do
				if [ -f "$STATE_DIR/$item" ] && [ -f "$STATE_DIR/$item-absent" ]; then
					die "conflicting backup presence metadata for $item"
				fi
				[ -f "$STATE_DIR/$item" ] || [ -f "$STATE_DIR/$item-absent" ] ||
					die "completed backup is missing $item"
			done
			[ -f "$STATE_DIR/failover-enabled" ] || die "completed backup is missing service state"
		fi
	fi
	# Only the first invocation decides whether wg0 was pre-existing; on a
	# re-run wg0 is ours and must not be reported as foreign.
	first_snapshot=0
	[ -f "$STATE_DIR/snapshot-complete" ] || first_snapshot=1

	mkdir -p "$STATE_DIR" || die "cannot create $STATE_DIR"
	chmod 700 "$STATE_DIR" 2>/dev/null || true
	# Explicitly mark the new format before copying any payload. A legacy
	# complete snapshot may be extended once, but damaged v2 metadata never is.
	snapshot_marker snapshot-version 3 || die "cannot record snapshot version"

	for cfg in network firewall dhcp; do
		if [ -f "$STATE_DIR/$cfg" ]; then
			msg "Reusing existing backup of /etc/config/$cfg"
		else
			copy_snapshot_file "/etc/config/$cfg" "$STATE_DIR/$cfg" ||
				die "cannot back up /etc/config/$cfg"
			chmod 600 "$STATE_DIR/$cfg" 2>/dev/null || true
			msg "Backed up /etc/config/$cfg"
		fi
	done
	# Optional configuration and managed files need presence markers as well
	# as copies: revert must distinguish originally absent from a lost backup.
	if [ ! -f "$STATE_DIR/kmwan-snapshot" ]; then
		if [ -f /etc/config/kmwan ]; then
			copy_snapshot_file /etc/config/kmwan "$STATE_DIR/kmwan" ||
				die "cannot back up kmwan"
			snapshot_marker kmwan-snapshot present || die "cannot record kmwan backup"
		else
			snapshot_marker kmwan-snapshot absent || die "cannot record absent kmwan"
		fi
	fi
	if [ ! -f "$STATE_DIR/managed-files-snapshot" ]; then
		for item in failover-worker failover-init; do
			case "$item" in
				failover-worker) path=$MONITOR ;;
				failover-init) path=$MONITOR_INIT ;;
			esac
			if [ -e "$path" ]; then
				copy_snapshot_file "$path" "$STATE_DIR/$item" ||
					die "cannot back up $path"
			elif [ ! -f "$STATE_DIR/$item" ]; then
				snapshot_marker "$item-absent" absent || die "cannot record absent $item"
			fi
		done
		if [ ! -f "$STATE_DIR/failover-enabled" ]; then
			if [ -x "$MONITOR_INIT" ] && "$MONITOR_INIT" enabled; then
				snapshot_marker failover-enabled 1 || die "cannot save service enabled state"
			else
				snapshot_marker failover-enabled 0 || die "cannot save service enabled state"
			fi
		fi
		if [ -x "$MONITOR_INIT" ] && "$MONITOR_INIT" running; then
			snapshot_marker failover-running 1 || die "cannot save service running state"
		else
			snapshot_marker failover-running 0 || die "cannot save service running state"
		fi
		snapshot_marker managed-files-snapshot complete || die "cannot complete managed file backups"
	fi

	if [ ! -f "$STATE_DIR/wan-sections" ]; then
		if [ -n "$WAN_SECTIONS" ]; then
			printf '%s\n' $WAN_SECTIONS > "$STATE_DIR/wan-sections"
		else
			: > "$STATE_DIR/wan-sections"
		fi
	fi
	if [ ! -f "$STATE_DIR/phones-file-snapshot" ]; then
		if [ -f "$PHONES_FILE" ]; then
			copy_snapshot_file "$PHONES_FILE" "$STATE_DIR/phones-config" || die "cannot back up phone configuration"
			snapshot_marker phones-file-snapshot present || die "cannot record phone configuration snapshot"
		else
			snapshot_marker phones-file-snapshot absent || die "cannot record absent phone configuration"
		fi
	fi

	if [ ! -f "$STATE_DIR/dns-upstream" ]; then
		{
			printf 'server=%s\n' \
				"$(uci -q get 'dhcp.@dnsmasq[0].server')"
			printf 'noresolv=%s\n' \
				"$(uci -q get 'dhcp.@dnsmasq[0].noresolv')"
		} > "$STATE_DIR/dns-upstream"
	fi

	if [ ! -f "$STATE_DIR/lan-ip" ]; then
		uci -q get 'network.lan.ipaddr' > "$STATE_DIR/lan-ip" 2>/dev/null ||
			: > "$STATE_DIR/lan-ip"
	fi

	# If wg0 already existed when we first snapshotted, say so: the operator
	# may have an older CellBridge (or other WireGuard) config in the backup.
	if [ "$first_snapshot" = 1 ] &&
		[ ! -f "$STATE_DIR/preexisting-wg0" ] &&
		[ -n "$(uci -q get "network.$WG_IF")" ]; then
		: > "$STATE_DIR/preexisting-wg0"
		warn "$WG_IF already exists; the snapshot includes that previous config"
	fi

	chmod 600 "$STATE_DIR"/* 2>/dev/null || true
	snapshot_marker snapshot-complete complete ||
		die "cannot write $STATE_DIR/snapshot-complete"
	chmod 600 "$STATE_DIR/snapshot-complete" 2>/dev/null || true
}

# Keep the QR endpoint stable on re-runs. An explicit --port always wins.
if [ "$WG_PORT_EXPLICIT" = 0 ]; then
	existing_port=$(uci -q get "network.$WG_IF.listen_port")
	case "$existing_port" in
		''|*[!0-9]*) : ;;
		*)
			if [ "$existing_port" -ge 1 ] && [ "$existing_port" -le 65535 ]; then
				WG_PORT=$existing_port
			fi
			;;
	esac
fi

msg "== CellBridge setup =="
msg "WireGuard UDP port: $WG_PORT"
msg "Snapshot directory: $STATE_DIR"
# Refuse a conflicting listener before the snapshot and before any config edit.
check_wg_port_available
ensure_failover_tools() {
	packages=
	command -v curl >/dev/null 2>&1 || packages="$packages curl"
	ip -Version 2>/dev/null | grep -q iproute2 || packages="$packages ip-full"
	[ -s /etc/ssl/certs/ca-certificates.crt ] || packages="$packages ca-bundle"
	if [ -n "$packages" ]; then
		opkg update || die "cannot refresh packages; no routing changes made"
		# Intentional splitting: packages is a list of fixed package names.
		opkg install $packages || die "cannot install failover tools; no routing changes made"
	fi
	command -v curl >/dev/null 2>&1 || die "curl is unavailable"
	ip -Version 2>/dev/null | grep -q iproute2 || die "ip-full is required for policy routing"
	[ -s /etc/ssl/certs/ca-certificates.crt ] || die "a CA bundle is required for HTTPS health checks"
}
# Numeric table inspection needs ip-full, which fresh OpenWrt may not have yet.
ensure_failover_tools
# Input: one IPv4 address/prefix or address/dotted-netmask per line.
# Reject any overlapping assignment, including a broader static LAN subnet.
subnets_are_free() {
	awk -v octet="$((10+index))" '
	function number(ip, a,n) {
		n=split(ip,a,"."); if(n!=4) return -1
		for(j=1;j<=4;j++) if(a[j]!~/^[0-9]+$/ || a[j]+0>255) return -1
		return ((a[1]*256+a[2])*256+a[3])*256+a[4]
	}
	BEGIN {target=number("10.10." octet ".0")}
	{
		split($1,p,"/"); address=number(p[1]); if(address<0) next
		if(p[2] ~ /\./) size=4294967296-number(p[2])
		else {bits=(p[2]=="" ? 32 : p[2]+0); if(bits==0) next; size=2^(32-bits)}
		if(size<=0) exit 1
		start=int(address/size)*size
		if(start<=target+255 && start+size-1>=target) exit 1
	}'
}
# Include configured but down phones in port checks.
for iface in $(uci -q show network | awk -F'[.=]' '/=interface$/ {print $2}'); do
	[ "$iface" = "$WG_IF" ] && continue
	[ "$(uci -q get "network.$iface.proto")" != wireguard ] ||
		[ "$(uci -q get "network.$iface.listen_port")" != "$WG_PORT" ] || die "UDP $WG_PORT is configured on $iface; choose --port"
	assignments=$(uci -q get "network.$iface.addresses")
	netmask=$(uci -q get "network.$iface.netmask")
	for address in $(uci -q get "network.$iface.ipaddr"); do
		case "$address" in */*) : ;; *) address=$address/${netmask:-24} ;; esac
		assignments="$assignments $address"
	done
	printf '%s\n' $assignments | subnets_are_free || die "tunnel subnet conflicts with $iface"
done
current_routes=$(ip -4 -N route show table all) || die "cannot inspect routing"
printf '%s\n' "$current_routes" | awk -v own="$WG_IF" '
	{for(i=1;i<NF;i++) if($i=="dev" && $(i+1)==own) next
	if($1 ~ /^[0-9]+\./) print $1; else if($1 ~ /^[0-9]+$/ && $2 ~ /^[0-9]+\./) print $2}' |
	subnets_are_free || die "tunnel subnet is already routed"
take_snapshot

if [ -n "$WAN_SECTIONS" ]; then
	msg "Detected WAN interface section(s): $WAN_SECTIONS"
else
	warn "no backup uplink interface detected"
fi

# ---------------------------------------------------------------------------
# WireGuard availability -- install dependencies before any routing changes.
# ---------------------------------------------------------------------------

ensure_wireguard() {
	if ! command -v wg >/dev/null 2>&1; then
		warn "wireguard-tools (wg) not found; trying to install it now"
		opkg update >/dev/null 2>&1 || true
		opkg install wireguard-tools >/dev/null 2>&1 || true
		command -v wg >/dev/null 2>&1 ||
			die "wireguard-tools is missing and could not be installed; WAN left enabled"
	fi

	# Creating a throwaway device is the only reliable test of *kernel*
	# support; built-in WireGuard has no kmod package to check for.
	modprobe wireguard >/dev/null 2>&1 || true
	ip link del dev cbtest0 >/dev/null 2>&1 || true
	if ip link add dev cbtest0 type wireguard >/dev/null 2>&1; then
		ip link del dev cbtest0 >/dev/null 2>&1 || true
		msg "WireGuard kernel support: present"
		return 0
	fi

	warn "kernel WireGuard not available; trying kmod-wireguard"
	opkg update >/dev/null 2>&1 || true
	opkg install kmod-wireguard >/dev/null 2>&1 || true
	modprobe wireguard >/dev/null 2>&1 || true

	ip link del dev cbtest0 >/dev/null 2>&1 || true
	if ip link add dev cbtest0 type wireguard >/dev/null 2>&1; then
		ip link del dev cbtest0 >/dev/null 2>&1 || true
		msg "WireGuard kernel support: installed"
		return 0
	fi

	die "kernel WireGuard support is unavailable and could not be installed; WAN left enabled"
}

# The QR is a convenience; never abort setup because this package is missing.
ensure_qrencode() {
	command -v qrencode >/dev/null 2>&1 && return 0
	warn "qrencode not found; trying to install it now"
	opkg update >/dev/null 2>&1 || true
	opkg install qrencode >/dev/null 2>&1 || true
	command -v qrencode >/dev/null 2>&1
}

ensure_wireguard
if ! ensure_qrencode; then
	warn "qrencode is unavailable; the phone settings will be printed as text"
fi

choose_phone_keys() {
	[ "$MANUAL_PHONE_KEY" = 0 ] || return 0
	if PHONE_PRIV=$(uci -q get "network.@wireguard_$WG_IF[0].cellbridge_phone_private_key"); then
		is_wireguard_key "$PHONE_PRIV" || die "stored generated phone key is invalid; refusing to rotate it"
		PHONE_PUB=$(printf '%s\n' "$PHONE_PRIV" | wg pubkey 2>/dev/null) ||
			die "stored generated phone key is unusable; refusing to rotate it"
		[ "$PHONE_PUB" = "$(uci -q get "network.@wireguard_$WG_IF[0].public_key")" ] ||
			die "stored generated phone keys do not match the configured peer; repair the pair or specify --phone-public-key"
		msg "Reusing generated phone keys for QR setup"
	else
		# Existing manual-key peers stay manual on an ordinary rerun.
		stored_pub=$(uci -q get "network.@wireguard_$WG_IF[0].public_key")
		if [ -n "$stored_pub" ]; then
			is_wireguard_key "$stored_pub" || die "stored phone public key is invalid; refusing to rotate it"
			PHONE_PUB=$stored_pub
			PHONE_PRIV=
			return 0
		fi
		PHONE_PRIV=$(wg genkey) || die "cannot generate phone keys"
		is_wireguard_key "$PHONE_PRIV" || die "generated phone key has an unexpected shape"
		PHONE_PUB=$(printf '%s\n' "$PHONE_PRIV" | wg pubkey 2>/dev/null) ||
			die "cannot derive the generated phone public key"
		msg "Generated phone keys for QR setup"
	fi
	is_wireguard_key "$PHONE_PUB" || die "generated phone public key has an unexpected shape"
}

# Check on EVERY run, including upgrades with an existing managed worker.
sh "$MONITOR_SOURCE" check || die "cannot inspect routing or reserved tables/rules contain foreign entries"
choose_phone_keys
if [ "$METADATA_ONLY" = 1 ] && [ "$NETWORK_OPTIONS" = 0 ] && [ "$ADD_PHONE" = 0 ] &&
	[ -n "$existing_record" ] && [ -x "$MONITOR_INIT" ] && [ -f "$PHONES_FILE" ] &&
	[ "$(uci -q get "network.$WG_IF.cellbridge_managed")" = 1 ] && cmp -s "$MONITOR_SOURCE" "$MONITOR"; then
	write_phones
	"$MONITOR_INIT" stop && "$MONITOR_INIT" start || die "cannot restart monitor"
	msg 'Phone preferences updated:'
	cat "$PHONES_FILE"
	exit 0
fi
# Record installation provenance separately from restored original managed files.
printf '2\n' > "$INSTALL_MARKER.new" && mv "$INSTALL_MARKER.new" "$INSTALL_MARKER" ||
	die "cannot record installation ownership"
rm -f "$RESTORED_MARKER" || die "cannot clear previous restore receipt"
# Stop a previously installed monitor only after dependencies and backups exist.
if [ -x "$MONITOR_INIT" ]; then
	"$MONITOR_INIT" stop || die "cannot stop the previous CellBridge monitor"
fi
if [ -x "$MONITOR" ]; then
	"$MONITOR" cleanup || die "cannot clean up the previous CellBridge routes"
fi

# ---------------------------------------------------------------------------
# wg0 interface and the phone peer
# ---------------------------------------------------------------------------

# Reuse the existing private key so re-running does not invalidate the phone's
# configuration.  The private key is never printed, logged or passed on a
# command line.
ensure_private_key() {
	# The private key is stored as an option on network.wg0, so the section
	# must exist first.  Create the shell of it here; configure_network fills
	# in the rest.  (On a fresh router, setting an option on a missing
	# section is an error in real uci.)
	if [ -z "$(uci -q get "network.$WG_IF")" ]; then
		uci set "network.$WG_IF=interface" ||
			die "uci: cannot create $WG_IF"
	fi

	wg_priv=$(uci -q get "network.$WG_IF.private_key")
	if [ -n "$wg_priv" ]; then
		if printf '%s\n' "$wg_priv" | wg pubkey >/dev/null 2>&1; then
			return 0
		fi
		warn "the existing $WG_IF private key is invalid; generating a new one"
	fi

	wg_priv=$(wg genkey) || die "wg genkey failed"
	case "$wg_priv" in
		*\'*) die "generated key contains an unexpected quote" ;;
	esac

	# Feed the key to uci over stdin so it does not appear in the process
	# argument list of a helper process.
	uci -q batch <<EOF || die "could not store the wg0 private key"
set network.$WG_IF.private_key='$wg_priv'
EOF
	msg "Generated a new $WG_IF private key"
}

configure_network() {
	msg "Configuring network.$WG_IF"
	uci set "network.$WG_IF=interface" || die "uci: cannot create $WG_IF"
	uci set "network.$WG_IF.proto=wireguard" || die "uci: cannot set proto"
	uci set "network.$WG_IF.cellbridge_managed=1" || die "uci: cannot mark interface ownership"
	uci set "network.$WG_IF.listen_port=$WG_PORT" || die "uci: cannot set port"
	uci set "network.$WG_IF.mtu=$WG_MTU" || die "uci: cannot set mtu"

	# The phone's endpoint is a LAN address reached through the connected
	# br-lan route.  No endpoint host route is needed, and disabling it keeps
	# netifd from pinning the endpoint to a physical backup uplink.
	uci set "network.$WG_IF.nohostroute=1"
	uci -q delete "network.$WG_IF.disabled"

	# Exactly one tunnel address; replace any stale list.
	uci -q delete "network.$WG_IF.addresses"
	uci add_list "network.$WG_IF.addresses=$WG_ADDR" ||
		die "uci: cannot set tunnel address"

	# Reconcile to exactly one peer: the phone.  Deleting and re-adding is
	# deliberate -- it removes duplicates and stale options from earlier or
	# hand-edited configs.  The handshake re-establishes within seconds.
	# Read the old preshared key first: a re-run must not rotate it, or an
	# already-configured phone would stop handshaking.
	had_peer=0
	uci -q get "network.@wireguard_$WG_IF[0]" >/dev/null 2>&1 && had_peer=1
	peer_psk=$(uci -q get "network.@wireguard_$WG_IF[0].preshared_key")
	if is_wireguard_key "$peer_psk"; then
		msg "Reusing the existing peer preshared key"
	else
		peer_psk=$(wg genpsk) || die "wg genpsk failed"
		is_wireguard_key "$peer_psk" ||
			die "generated preshared key has an unexpected shape"
		msg "Generated a new peer preshared key"
		if [ "$had_peer" = 1 ]; then
			warn "the existing peer had no usable preshared key; update any previously configured phone with the new PresharedKey or re-scan the QR"
		fi
	fi

	while uci -q delete "network.@wireguard_$WG_IF[0]" >/dev/null 2>&1; do
		:
	done

	peer=$(uci add network "wireguard_$WG_IF") ||
		die "uci: cannot add the WireGuard peer"

	uci set "network.$peer.public_key=$PHONE_PUB" ||
		die "uci: cannot set the peer public key"
	# Feed the key to uci over stdin so it does not appear in a helper
	# process's argument list.
	uci -q batch <<EOF || die "uci: cannot set the preshared key"
set network.$peer.preshared_key='$peer_psk'
EOF
	if [ -n "$PHONE_PRIV" ]; then
		# CellBridge-only metadata: netifd ignores this option. Keeping it with
		# the peer makes re-runs reproducible and covers it with network backup.
		uci -q batch <<EOF || die "uci: cannot retain generated phone keys"
set network.$peer.cellbridge_phone_private_key='$PHONE_PRIV'
EOF
	fi
	# Keep broad cryptokey routing, but let internet health own the default.
	uci set "network.$peer.allowed_ips=0.0.0.0/0" ||
		die "uci: cannot set allowed_ips"
	uci set "network.$peer.route_allowed_ips=0" ||
		die "uci: cannot set route_allowed_ips"
	uci set "network.$peer.persistent_keepalive=25" || die "uci: cannot set keepalive"

	uci commit network || die "uci commit network failed"
	msg "Peer: $PHONE_PUB (allowed_ips 0.0.0.0/0, health-controlled default route)"
}

configure_backup_priority() {
	msg "Keeping kmwan enabled: USB tether first, WAN second"
	uci set kmwan.global.mode=failover || die "cannot set kmwan failover mode"
	for sec in $(uci -q show kmwan | awk -F'[.=]' '/=member$/ { print $2 }'); do
		[ "$(uci -q get "kmwan.$sec.addr_type")" = 4 ] || continue
		iface=$(uci -q get "kmwan.$sec.interface")
		case "$iface" in
			tethering) metric=1 ;;
			wan) metric=2 ;;
			*) continue ;;
		esac
		uci set "kmwan.$sec.metric=$metric" || die "cannot set backup member priority"
		uci set "kmwan.$sec.disabled=0" || die "cannot enable backup member"
		if [ -n "$(uci -q get "network.$iface")" ]; then
			uci set "network.$iface.metric=$metric" || die "cannot set backup route priority"
		fi
	done
	# Upgrading from an older fail-closed installation: only re-enable WANs
	# that were not disabled in the ORIGINAL snapshot, not user-disabled ones.
	for iface in $WAN_SECTIONS; do
		if [ "$(uci -q -c "$STATE_DIR" get "network.$iface.disabled")" != 1 ]; then
			uci -q delete "network.$iface.disabled" || true
		fi
	done
	uci commit kmwan || die "cannot commit backup priorities"
	uci commit network || die "cannot commit backup routes"
}

install_failover_monitor() {
	cp "$MONITOR_SOURCE" "$MONITOR.new" && cmp -s "$MONITOR_SOURCE" "$MONITOR.new" &&
		chmod 700 "$MONITOR.new" && mv "$MONITOR.new" "$MONITOR" || die "cannot install tunnel monitor"
	cat > "$MONITOR_INIT.new" <<'EOF' || die "cannot install monitor service"
#!/bin/sh /etc/rc.common
# CellBridge-managed tunnel health service; kmwan retains backup management.
START=95
STOP=10
USE_PROCD=1
start_service() {
	procd_open_instance
	procd_set_param command /usr/sbin/cellbridge-failover run
	procd_set_param respawn 3600 5 0
	procd_set_param term_timeout 10
	procd_set_param stdout 1
	procd_set_param stderr 1
	procd_close_instance
}
service_stopped() {
	[ ! -x /usr/sbin/cellbridge-failover ] || /usr/sbin/cellbridge-failover cleanup
}
EOF
	chmod 700 "$MONITOR_INIT.new" && mv "$MONITOR_INIT.new" "$MONITOR_INIT" || die "cannot protect monitor service"
	"$MONITOR_INIT" enable || die "cannot enable monitor at boot"
}

# Print the section name of the firewall zone called "vpn", if any.
find_zone_by_name() {
	for sec in $(uci -q show firewall |
		awk -F'[.=]' '/=zone$/ { print $2 }'); do
		if [ "$(uci -q get "firewall.$sec.name")" = "$1" ]; then
			printf '%s\n' "$sec"
			return 0
		fi
	done
	return 1
}

configure_firewall() {
	msg "Configuring the 'vpn' firewall zone"

	zone=$(find_zone_by_name vpn) || zone=
	if [ -z "$zone" ]; then
		zone=cellbridge_vpn
		uci set "firewall.$zone=zone" || die "uci: cannot create the vpn zone"
		uci set "firewall.$zone.cellbridge_managed=1" || die "cannot mark firewall zone"
		msg "Created firewall zone section '$zone' (name 'vpn')"
	else
		msg "Reusing existing firewall zone '$zone' (name 'vpn')"
	fi

	uci set "firewall.$zone.name=vpn"
	uci set "firewall.$zone.input=ACCEPT"
	uci set "firewall.$zone.output=ACCEPT"
	# Return traffic is conntracked; vpn->other-zone forwarding is not wanted.
	uci set "firewall.$zone.forward=REJECT"
	uci set "firewall.$zone.masq=1"
	uci set "firewall.$zone.mtu_fix=1"

	# Make sure wg0 is covered without dropping any other network that an
	# existing zone of the same name may already have.
	have_net=0
	for n in $(uci -q get "firewall.$zone.network"); do
		[ "$n" = "$WG_IF" ] && have_net=1
	done
	if [ "$have_net" = 0 ]; then
		uci add_list "firewall.$zone.network=$WG_IF" ||
			die "uci: cannot add $WG_IF to the vpn zone"
	fi

	# Forwarding lan -> vpn.  Reuse a matching existing section if there is
	# one, otherwise create a stable named section.
	fwd=
	for sec in $(uci -q show firewall |
		awk -F'[.=]' '/=forwarding$/ { print $2 }'); do
		if [ "$(uci -q get "firewall.$sec.src")" = lan ] &&
			[ "$(uci -q get "firewall.$sec.dest")" = vpn ] &&
			[ "$(uci -q get "firewall.$sec.enabled")" != 0 ]; then
			fwd=$sec
		fi
	done
	if [ -z "$fwd" ]; then
		fwd=cellbridge_fwd
		uci set "firewall.$fwd=forwarding" ||
			die "uci: cannot create the lan->vpn forwarding"
		uci set "firewall.$fwd.cellbridge_managed=1" || die "cannot mark forwarding"
		uci set "firewall.$fwd.src=lan"
		uci set "firewall.$fwd.dest=vpn"
		uci set "firewall.$fwd.enabled=1"
	fi

	uci commit firewall || die "uci commit firewall failed"
	msg "Zone vpn: masquerade + MSS clamping on; lan -> vpn forwarding enabled"
}

configure_dns() {
	msg "Pointing dnsmasq upstream at: $DNS_LIST"

	# Public resolvers follow the selected path, including USB/WAN fallback.
	uci set 'dhcp.@dnsmasq[0].noresolv=1' ||
		die "uci: cannot set noresolv"
	uci -q delete 'dhcp.@dnsmasq[0].server'
	for d in $DNS_LIST; do
		uci add_list "dhcp.@dnsmasq[0].server=$d" ||
			die "uci: cannot add DNS server $d"
	done
	uci commit dhcp || die "uci commit dhcp failed"
}

# Restrict the existing file BEFORE committing any generated private material.
chmod 600 /etc/config/network || die "cannot protect network key storage"
ensure_private_key
configure_network
[ "$ADD_PHONE" = 1 ] || [ "$WG_IF" != wg0 ] || configure_backup_priority
configure_firewall
if [ "$DNS_EXPLICIT" = 1 ] || { [ "$ADD_PHONE" = 0 ] && [ "$WG_IF" = wg0 ]; }; then configure_dns; fi
write_phones
install_failover_monitor

# ---------------------------------------------------------------------------
# Apply and verify wg0 before starting health-controlled tunnel preference
# ---------------------------------------------------------------------------

# Reload the network and wait until the tunnel address exists.  This is the
# cheap check that netifd, wireguard-tools and the kernel module all agree.
wg0_ready() {
	ip -4 addr show dev "$WG_IF" 2>/dev/null |
		grep -q "$WG_ADDR_HOST/" &&
		wg0_link_up &&
		[ "$(wg show "$WG_IF" listen-port 2>/dev/null)" = "$WG_PORT" ]
}

bring_up_wg() {
	if [ "$ADD_PHONE" = 1 ] || [ "$WG_IF" != wg0 ]; then
		ifup "$WG_IF" >/dev/null 2>&1 || true
	else
		/etc/init.d/network reload >/dev/null 2>&1 || true
	fi

	# An address can exist while the link is still down (for example when the
	# UDP port was rejected). ifup also repairs netifd's stale "up" state.
	wg0_link_up || ifup "$WG_IF" >/dev/null 2>&1 || true

	i=0
	while [ "$i" -lt 15 ]; do
		if wg0_ready; then
			return 0
		fi
		# A changed port or a stale device may need one explicit re-up.
		if [ "$i" = 2 ]; then
			ifup "$WG_IF" >/dev/null 2>&1 || true
		fi
		sleep 1
		i=$((i + 1))
	done
	return 1
}

msg "Reloading the network"
if ! bring_up_wg; then
	warn "$WG_IF did not become ready (address, link, UDP $WG_PORT) within 15s"
	warn "wg0 link: $(ip link show "$WG_IF" 2>/dev/null | head -n 1)"
	warn "wg0 listen-port: $(wg show "$WG_IF" listen-port 2>/dev/null)"
	warn "recent WireGuard/interface log lines:"
	logread 2>/dev/null | grep -i -E 'wireguard|wg0|netifd' | tail -n 8 >&2 || true
	die "$WG_IF did not come up; backups remain enabled; run cellbridge-revert.sh to restore"
fi
msg "wg0 is up with $WG_ADDR"

# Reload the firewall after wg0 exists so the zone binds to a real interface.
if command -v fw4 >/dev/null 2>&1; then
	fw4 reload >/dev/null 2>&1 || warn "fw4 reload returned an error"
else
	/etc/init.d/firewall reload >/dev/null 2>&1 ||
		warn "firewall reload returned an error"
fi

if [ "$DNS_EXPLICIT" = 1 ] || { [ "$ADD_PHONE" = 0 ] && [ "$WG_IF" = wg0 ]; }; then
	/etc/init.d/dnsmasq restart >/dev/null 2>&1 || warn "dnsmasq restart returned an error; DNS may need a manual restart"
fi

# Read-only check: the phone's handshake arrives on br-lan, so the LAN zone's
# input policy governs UDP 51820.  Never change that policy here -- it is the
# path the operator's SSH session uses.
lan_zone=$(find_zone_by_name lan) || lan_zone=
if [ -n "$lan_zone" ]; then
	lan_input=$(uci -q get "firewall.$lan_zone.input")
	if [ "$lan_input" != ACCEPT ]; then
		warn "the LAN firewall zone input policy is '$lan_input'; the phone's"
		configured_ports=$(awk '{printf "%s%s", separator, $3; separator=", "}' "$PHONES_FILE")
		warn "handshakes to UDP ports $configured_ports may be dropped. Add allow rules for"
		warn "the LAN zone rather than changing the zone policy (SSH uses it)."
	fi
fi

# ---------------------------------------------------------------------------
# Keep the native physical-uplink controller and start tunnel health routing.
# ---------------------------------------------------------------------------

[ "$ADD_PHONE" = 1 ] || [ "$WG_IF" != wg0 ] || "$MONITOR" restart-kmwan || die "kmwan restart failed runtime checks; run revert to restore"
"$MONITOR_INIT" start || die "cannot start tunnel monitor; run revert to restore"

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------

router_pub=$(wg show "$WG_IF" public-key 2>/dev/null) || router_pub=
if [ -z "$router_pub" ]; then
	router_pub=$(uci -q get "network.$WG_IF.private_key" |
		wg pubkey 2>/dev/null) || router_pub=
fi

[ "$router_pub" = "$PHONE_PUB" ] &&
	warn "the phone public key equals the router public key; the phone config is wrong"

lan_ip=$(uci -q get 'network.lan.ipaddr')
lan_ip=${lan_ip%% *}
lan_ip=${lan_ip%%/*}
if [ -z "$lan_ip" ]; then
	lan_ip=$(ip -4 addr show dev "$LAN_DEV" 2>/dev/null |
		sed -n 's/.*inet \([0-9][0-9.]*\)\/.*/\1/p' | head -n 1)
fi
[ -n "$lan_ip" ] || lan_ip=192.168.8.1

if [ "$APP_OUTPUT" = 1 ]; then
	[ -n "$router_pub" ] || die "router public key is unavailable"
	# Return before the manual/QR summary, which intentionally displays secrets.
	printf 'CELLBRIDGE1:{"address":"%s","peerPublicKey":"%s","presharedKey":"%s","endpoint":"%s:%s","mtu":%s,"phonePublicKey":"%s"}\n' \
		"$PHONE_ADDR" "$router_pub" "$peer_psk" "$lan_ip" "$WG_PORT" "$WG_MTU" "$PHONE_PUB" >&3
	exit 0
fi

msg ""
msg "== CellBridge is configured =="
msg "Router WireGuard public key: ${router_pub:-UNAVAILABLE (run: wg show $WG_IF public-key)}"
msg "Phone endpoint:              $lan_ip:$WG_PORT (UDP)"
msg "Tunnel addresses:            router $WG_ADDR, phone $PHONE_ADDR"
msg "DNS upstream (selected path): $(uci -q get 'dhcp.@dnsmasq[0].server')"
msg "Backup directory:            $STATE_DIR"
msg ""
msg "Phone WireGuard settings:"
if [ -n "$PHONE_PRIV" ]; then
	msg "  PrivateKey = (included in the QR; retained root-only for re-runs)"
fi
msg "  Address    = $PHONE_ADDR"
msg "  PublicKey  = ${router_pub:-<router public key>}"
msg "  PresharedKey = $peer_psk"
msg "  Endpoint   = $lan_ip:$WG_PORT"
msg "  MTU        = $WG_MTU"
msg "  AllowedIPs = 0.0.0.0/0"
msg ""
if [ -z "$router_pub" ]; then
	[ "$APP_OUTPUT" = 0 ] || die "cannot render the setup payload: the router public key is unavailable"
	warn "cannot render the phone QR: the router public key is unavailable"
else
	phone_key_json=
	if [ -n "$PHONE_PRIV" ]; then
		phone_key_json=$(printf ',"phonePrivateKey":"%s"' "$PHONE_PRIV")
	fi
	qr_payload=$(printf 'CELLBRIDGE1:{"address":"%s","peerPublicKey":"%s","presharedKey":"%s","endpoint":"%s:%s","mtu":%s,"phonePublicKey":"%s"%s}' \
		"$PHONE_ADDR" "$router_pub" "$peer_psk" "$lan_ip" "$WG_PORT" "$WG_MTU" "$PHONE_PUB" "$phone_key_json")

	msg "Scan this QR code in the CellBridge app:"
	qr_art=
	if command -v qrencode >/dev/null 2>&1; then
		qr_art=$(printf '%s' "$qr_payload" | qrencode -t ANSIUTF8 -m 2 2>/dev/null) || qr_art=
		if [ -z "$qr_art" ]; then
			qr_art=$(printf '%s' "$qr_payload" | qrencode -t UTF8 -m 2 2>/dev/null) || qr_art=
		fi
	fi
	if [ -n "$qr_art" ]; then
		msg "$qr_art"
	else
		warn "could not render a QR code; use the text settings above"
		msg "CellBridge setup payload:"
		msg "$qr_payload"
	fi
fi

msg "Backup state (priority: USB tether, then WAN):"
for w in $WAN_SECTIONS; do
	msg "  $w: disabled=$(uci -q get "network.$w.disabled")"
done

msg "Default route(s) now:"
ip route show 2>/dev/null | grep '^default' | sed 's/^/  /'
msg ""
msg "Once the phone app is connected, check the handshake with:"
msg "  wg show $WG_IF"
msg ""
msg "To undo everything:"
msg "  sh /root/cellbridge-revert.sh"
msg ""
msg "Tunnel internet is preferred after two good HTTPS checks; three failures select backups."
msg "USB is monitored by kmwan only when attached; no additional USB polling is installed."
msg "Monitor status: /usr/sbin/cellbridge-failover status"
msg "Monitor logs: logread -e cellbridge-failover"
msg 'Configured phones (lower priority number is preferred):'
cat "$PHONES_FILE"
