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
# The router is the WireGuard *server* (wg0, 10.10.10.1/24, UDP 51820).  The
# phone is the only peer (10.10.10.2/24) and the *initiator*: it is a Wi-Fi
# client of this router, so no static DHCP lease or fixed LAN address is
# needed.  The router sends the LAN's default traffic into the tunnel and
# masquerades it, so the phone sees every LAN client as 10.10.10.1.
#
# This script is fail-closed by design: the old WAN is disabled and there is
# no backup uplink.  If the phone is away the LAN has no internet until the
# phone returns or cellbridge-revert.sh is run.  SSH on the LAN stays up.
#
# Route note -- do not "fix" this:
#   `route_allowed_ips 1` with `allowed_ips 0.0.0.0/0` installs a default
#   route (`default dev wg0`) in the main table.  The LAN's connected route
#   `192.168.8.0/24 dev br-lan` is more specific, so the phone's own LAN
#   address -- which is the WireGuard endpoint -- is still reached through
#   br-lan.  The encapsulated UDP packets therefore do not recurse into the
#   tunnel, and the tunnel comes up without a route to its own endpoint.
#   Do not replace the LAN route with a broader one and do not route the
#   phone's LAN address into wg0.
#
# The same `allowed_ips 0.0.0.0/0` is also the inbound source filter: replies
# from the internet arrive from arbitrary addresses and must be accepted from
# the phone.  Narrowing it would silently drop those replies.
#
# Usage:
#   sh cellbridge-setup.sh --phone-public-key '<base64 public key>'
#
# Options:
#   --phone-public-key KEY   phone's WireGuard *public* key (required)
#   --dns ADDR               upstream resolver for dnsmasq (repeatable,
#                            default 1.1.1.1)
#   --wan-iface NAME         extra UCI interface to disable (repeatable);
#                            normally auto-detected
#   --keep-wan               configure the tunnel but leave the WAN enabled
#                            (rehearsal only: the default route still moves
#                            to wg0, so this is not a guaranteed fallback)
#   -h, --help               this text
#
# The phone's *private* key must never be passed to this script or stored on
# the router.  Only the public key is accepted.
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
WG_MTU=1420
PHONE_ADDR=10.10.10.2/24
DEFAULT_DNS=1.1.1.1
LAN_DEV=br-lan

msg()  { printf '%s\n' "$*"; }
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

usage() {
	cat <<'EOF'
CellBridge router setup (GL.iNet Flint 3 / OpenWrt fw4)

Usage:
  sh cellbridge-setup.sh --phone-public-key '<base64 public key>' [options]

Options:
  --phone-public-key KEY   phone WireGuard public key (required)
  --dns ADDR               upstream resolver (repeatable, default 1.1.1.1)
  --wan-iface NAME         extra WAN UCI interface to disable (repeatable)
  --keep-wan               do not disable the WAN (rehearsal; not a fallback)
  -h, --help               show this help

The phone's private key must never be given to this script.

It prints the phone's WireGuard settings, including the generated or reused
preshared key, plus a QR code for the CellBridge app.  Setup installs
`qrencode` while the WAN is up; if that fails, the text settings and the raw
payload are printed instead.
EOF
}

# ---------------------------------------------------------------------------
# Arguments
# ---------------------------------------------------------------------------

PHONE_PUB=${CELLBRIDGE_PHONE_PUBLIC_KEY:-}
DNS_LIST=
WAN_OVERRIDES=
KEEP_WAN=0

while [ $# -gt 0 ]; do
	case "$1" in
	--phone-public-key)
		[ $# -ge 2 ] || die "--phone-public-key needs a value"
		PHONE_PUB=$2
		shift 2
		;;
	--phone-public-key=*)
		PHONE_PUB=${1#*=}
		shift
		;;
	--dns)
		[ $# -ge 2 ] || die "--dns needs a value"
		DNS_LIST="$DNS_LIST $2"
		shift 2
		;;
	--dns=*)
		DNS_LIST="$DNS_LIST ${1#*=}"
		shift
		;;
	--wan-iface)
		[ $# -ge 2 ] || die "--wan-iface needs a value"
		WAN_OVERRIDES="$WAN_OVERRIDES $2"
		shift 2
		;;
	--wan-iface=*)
		WAN_OVERRIDES="$WAN_OVERRIDES ${1#*=}"
		shift
		;;
	--keep-wan)
		KEEP_WAN=1
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

[ -n "$DNS_LIST" ] || DNS_LIST=$DEFAULT_DNS

# Strip whitespace the operator may have pasted along with the key.
PHONE_PUB=$(printf '%s' "$PHONE_PUB" | tr -d '[:space:]')
[ -n "$PHONE_PUB" ] || {
	usage >&2
	die "--phone-public-key is required"
}

# A WireGuard key is 32 bytes -> 44 base64 characters ending in '='.
PHONE_PUB_LEN=$(printf '%s' "$PHONE_PUB" | wc -c | tr -d ' ')
[ "$PHONE_PUB_LEN" = 44 ] ||
	die "the phone public key must be 44 base64 characters (got $PHONE_PUB_LEN)"
case "$PHONE_PUB" in
	*=) : ;;
	*) die "the phone public key must end with '='" ;;
esac
case "$PHONE_PUB" in
	*[!A-Za-z0-9+/=]*) die "the phone public key contains non-base64 characters" ;;
esac

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

uci -q get 'dhcp.@dnsmasq[0]' >/dev/null 2>&1 ||
	die "no dnsmasq section in /etc/config/dhcp; refusing to guess"

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
			wan*|wwan*|usbwan*|*_wan) printf '%s\n' "$sec" ;;
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

# Take the snapshot exactly once and never overwrite an earlier one.  If a
# previous run was interrupted after copying some files, the missing files are
# filled in; a file that is already present is from an earlier (closer to the
# original) point in time and is kept.
take_snapshot() {
	# Only the first invocation decides whether wg0 was pre-existing; on a
	# re-run wg0 is ours and must not be reported as foreign.
	first_snapshot=0
	[ -f "$STATE_DIR/snapshot-complete" ] || first_snapshot=1

	mkdir -p "$STATE_DIR" || die "cannot create $STATE_DIR"
	chmod 700 "$STATE_DIR" 2>/dev/null || true

	for cfg in network firewall dhcp; do
		if [ -f "$STATE_DIR/$cfg" ]; then
			msg "Reusing existing backup of /etc/config/$cfg"
		else
			cp -p "/etc/config/$cfg" "$STATE_DIR/$cfg" ||
				die "cannot back up /etc/config/$cfg"
			chmod 600 "$STATE_DIR/$cfg" 2>/dev/null || true
			msg "Backed up /etc/config/$cfg"
		fi
	done

	if [ ! -f "$STATE_DIR/wan-sections" ]; then
		if [ -n "$WAN_SECTIONS" ]; then
			printf '%s\n' $WAN_SECTIONS > "$STATE_DIR/wan-sections"
		else
			: > "$STATE_DIR/wan-sections"
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
	: > "$STATE_DIR/snapshot-complete" ||
		die "cannot write $STATE_DIR/snapshot-complete"
	chmod 600 "$STATE_DIR/snapshot-complete" 2>/dev/null || true
}

msg "== CellBridge setup =="
msg "Snapshot directory: $STATE_DIR"
take_snapshot

if [ -n "$WAN_SECTIONS" ]; then
	msg "Detected WAN interface section(s): $WAN_SECTIONS"
else
	warn "no WAN interface detected; nothing will be disabled"
fi

# ---------------------------------------------------------------------------
# WireGuard availability -- before the WAN is disabled, because afterwards
# there is no way to install packages.
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
	uci set "network.$WG_IF.listen_port=$WG_PORT" || die "uci: cannot set port"
	uci set "network.$WG_IF.mtu=$WG_MTU" || die "uci: cannot set mtu"

	# The phone's endpoint is a LAN address reached through the connected
	# br-lan route.  No endpoint host route is needed, and disabling it keeps
	# netifd from pinning the endpoint to the WAN we are about to disable.
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
	uci -q get 'network.@wireguard_wg0[0]' >/dev/null 2>&1 && had_peer=1
	peer_psk=$(uci -q get 'network.@wireguard_wg0[0].preshared_key')
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

	while uci -q delete "network.@wireguard_wg0[0]" >/dev/null 2>&1; do
		:
	done

	peer=$(uci add network wireguard_wg0) ||
		die "uci: cannot add the WireGuard peer"

	uci set "network.$peer.public_key=$PHONE_PUB" ||
		die "uci: cannot set the peer public key"
	# Feed the key to uci over stdin so it does not appear in a helper
	# process's argument list.
	uci -q batch <<EOF || die "uci: cannot set the preshared key"
set network.$peer.preshared_key='$peer_psk'
EOF
	# 0.0.0.0/0 is both the default route and the inbound source filter.
	uci set "network.$peer.allowed_ips=0.0.0.0/0" ||
		die "uci: cannot set allowed_ips"
	uci set "network.$peer.route_allowed_ips=1" ||
		die "uci: cannot set route_allowed_ips"

	uci commit network || die "uci commit network failed"
	msg "Peer: $PHONE_PUB (allowed_ips 0.0.0.0/0, route_allowed_ips 1)"
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
			[ "$(uci -q get "firewall.$sec.dest")" = vpn ]; then
			fwd=$sec
		fi
	done
	if [ -z "$fwd" ]; then
		fwd=cellbridge_fwd
		uci set "firewall.$fwd=forwarding" ||
			die "uci: cannot create the lan->vpn forwarding"
	fi
	uci set "firewall.$fwd.src=lan"
	uci set "firewall.$fwd.dest=vpn"
	uci set "firewall.$fwd.enabled=1"

	uci commit firewall || die "uci commit firewall failed"
	msg "Zone vpn: masquerade + MSS clamping on; lan -> vpn forwarding enabled"
}

configure_dns() {
	msg "Pointing dnsmasq upstream at: $DNS_LIST"

	# Ignore /tmp/resolv.conf.auto: it is populated by the WAN, which is about
	# to be disabled, and would leave dnsmasq without an upstream.  DNS then
	# travels through the tunnel like any other UDP.
	uci set 'dhcp.@dnsmasq[0].noresolv=1' ||
		die "uci: cannot set noresolv"
	uci -q delete 'dhcp.@dnsmasq[0].server'
	for d in $DNS_LIST; do
		uci add_list "dhcp.@dnsmasq[0].server=$d" ||
			die "uci: cannot add DNS server $d"
	done
	uci commit dhcp || die "uci commit dhcp failed"
}

ensure_private_key
configure_network
configure_firewall
configure_dns

# ---------------------------------------------------------------------------
# Apply and verify wg0 before fail-closing the WAN
# ---------------------------------------------------------------------------

# Reload the network and wait until the tunnel address exists.  This is the
# cheap check that netifd, wireguard-tools and the kernel module all agree.
bring_up_wg() {
	/etc/init.d/network reload >/dev/null 2>&1 || true

	# A brand-new interface is sometimes not brought up by a reload alone.
	if ! ip link show "$WG_IF" >/dev/null 2>&1; then
		ifup "$WG_IF" >/dev/null 2>&1 || true
	fi

	i=0
	while [ "$i" -lt 15 ]; do
		if ip -4 addr show dev "$WG_IF" 2>/dev/null |
			grep -q "$WG_ADDR_HOST/"; then
			return 0
		fi
		sleep 1
		i=$((i + 1))
	done
	return 1
}

msg "Reloading the network"
if ! bring_up_wg; then
	warn "wg0 did not get $WG_ADDR within 15s"
	warn "recent WireGuard/interface log lines:"
	logread 2>/dev/null | grep -i -E 'wireguard|wg0|netifd' | tail -n 8 >&2 || true
	die "refusing to disable the WAN while $WG_IF is not up; run cellbridge-revert.sh to restore"
fi
msg "wg0 is up with $WG_ADDR"

# Reload the firewall after wg0 exists so the zone binds to a real interface.
if command -v fw4 >/dev/null 2>&1; then
	fw4 reload >/dev/null 2>&1 || warn "fw4 reload returned an error"
else
	/etc/init.d/firewall reload >/dev/null 2>&1 ||
		warn "firewall reload returned an error"
fi

/etc/init.d/dnsmasq restart >/dev/null 2>&1 ||
	warn "dnsmasq restart returned an error; DNS may need a manual restart"

# Read-only check: the phone's handshake arrives on br-lan, so the LAN zone's
# input policy governs UDP 51820.  Never change that policy here -- it is the
# path the operator's SSH session uses.
lan_zone=$(find_zone_by_name lan) || lan_zone=
if [ -n "$lan_zone" ]; then
	lan_input=$(uci -q get "firewall.$lan_zone.input")
	if [ "$lan_input" != ACCEPT ]; then
		warn "the LAN firewall zone input policy is '$lan_input'; the phone's"
		warn "handshake to UDP $WG_PORT may be dropped. Add an allow rule for"
		warn "the LAN zone rather than changing the zone policy (SSH uses it)."
	fi
fi

# ---------------------------------------------------------------------------
# Fail closed: disable the old WAN uplink(s)
# ---------------------------------------------------------------------------

if [ "$KEEP_WAN" = 1 ]; then
	msg "Leaving the WAN enabled (--keep-wan)"
elif [ -z "$WAN_SECTIONS" ]; then
	warn "no WAN interface to disable; the LAN may still have a backup uplink"
else
	for w in $WAN_SECTIONS; do
		name=$(uci -q get "network.$w.name")
		[ -n "$name" ] || name=$w
		uci set "network.$w.disabled=1" ||
			die "uci: cannot disable WAN interface $name"
		msg "Disabled WAN interface: $name"
	done
	uci commit network || die "uci commit network failed"

	/etc/init.d/network reload >/dev/null 2>&1 || true
	for w in $WAN_SECTIONS; do
		ifdown "$w" >/dev/null 2>&1 || true
	done
fi

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

msg ""
msg "== CellBridge is configured =="
msg "Router WireGuard public key: ${router_pub:-UNAVAILABLE (run: wg show $WG_IF public-key)}"
msg "Phone endpoint:              $lan_ip:$WG_PORT (UDP)"
msg "Tunnel addresses:            router $WG_ADDR, phone $PHONE_ADDR"
msg "DNS upstream (via tunnel):   $(printf '%s' "$DNS_LIST" | sed 's/^ //')"
msg "Backup directory:            $STATE_DIR"
msg ""
msg "Phone WireGuard settings:"
msg "  Address    = $PHONE_ADDR"
msg "  PublicKey  = ${router_pub:-<router public key>}"
msg "  PresharedKey = $peer_psk"
msg "  Endpoint   = $lan_ip:$WG_PORT"
msg "  MTU        = $WG_MTU"
msg "  AllowedIPs = 0.0.0.0/0"
msg ""

if [ -z "$router_pub" ]; then
	warn "cannot render the phone QR: the router public key is unavailable"
else
	qr_payload=$(printf 'CELLBRIDGE1:{"address":"%s","peerPublicKey":"%s","presharedKey":"%s","endpoint":"%s:%s","mtu":%s,"phonePublicKey":"%s"}' \
		"$PHONE_ADDR" "$router_pub" "$peer_psk" "$lan_ip" "$WG_PORT" "$WG_MTU" "$PHONE_PUB")

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

if [ "$KEEP_WAN" = 1 ]; then
	msg "WAN state: --keep-wan (this run did not change it)"
	for w in $WAN_SECTIONS; do
		name=$(uci -q get "network.$w.name")
		[ -n "$name" ] || name=$w
		msg "  $name: disabled=$(uci -q get "network.$w.disabled")"
	done
else
	msg "WAN state:"
	for w in $WAN_SECTIONS; do
		name=$(uci -q get "network.$w.name")
		[ -n "$name" ] || name=$w
		msg "  $name: disabled=$(uci -q get "network.$w.disabled")"
	done
	[ -n "$WAN_SECTIONS" ] || msg "  (no WAN interface detected)"
fi

msg "Default route(s) now:"
ip route show 2>/dev/null | grep '^default' | sed 's/^/  /'
msg ""
msg "Once the phone app is connected, check the handshake with:"
msg "  wg show $WG_IF"
msg ""
msg "To undo everything:"
msg "  sh /root/cellbridge-revert.sh"
msg ""
msg "Remember: no backup uplink. If the phone is away, the LAN has no internet."
