#!/bin/sh
# CellBridge app provisioning helper. Runs on the router beside the other
# CellBridge scripts. The phone app uploads all four scripts and invokes this
# wrapper; it never needs SFTP and the router never sees the phone's private
# key. POSIX sh / BusyBox ash only.
#
#   sh cellbridge-provision.sh check-internet
#       Exit 0 when the router has working internet, 75 when it does not.
#       Callers must wait and retry rather than proceeding.
#
#   sh cellbridge-provision.sh register PHONE_PUBLIC_KEY PRIORITY
#       Serialize registration, verify internet, then configure or reuse the
#       phone whose public key matches. Prints diagnostics on stderr and one
#       CELLBRIDGE1: setup payload on stdout. PRIORITY is 1-10, 1 highest.
set -u

SCRIPT_DIR=$(CDPATH= cd "$(dirname "$0")" && pwd) || exit 1
SETUP=$SCRIPT_DIR/cellbridge-setup.sh
MONITOR=$SCRIPT_DIR/cellbridge-failover.sh
REVERT=$SCRIPT_DIR/cellbridge-revert.sh
LOCK_DIR=${CELLBRIDGE_PROVISION_LOCK:-/var/run/cellbridge-provision.lock}

HEALTH_URLS='https://1.1.1.1/cdn-cgi/trace https://1.0.0.1/cdn-cgi/trace'

log() { printf '%s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

internet_works() {
	for url in $HEALTH_URLS; do
		if command -v curl >/dev/null 2>&1; then
			code=$(curl --ipv4 --noproxy '*' --silent --fail \
				--connect-timeout 3 --max-time 8 \
				--output /dev/null --write-out '%{http_code}' "$url" 2>/dev/null) || continue
			[ "$code" = 200 ] && return 0
		elif command -v wget >/dev/null 2>&1; then
			# BusyBox wget has no --write-out; a successful TLS fetch is enough.
			wget -q -O /dev/null --timeout=8 "$url" 2>/dev/null && return 0
		else
			die "neither curl nor wget is available for the internet check"
		fi
	done
	return 1
}

check_internet() {
	internet_works && { log 'Router internet is available'; return 0; }
	log 'Router internet is unavailable; waiting for the user to restore it'
	return 75
}

acquire_lock() {
	# BusyBox often lacks flock; an atomic mkdir is a portable mutex. A stale
	# lock from a killed run is reported so the operator can remove it.
	if ! mkdir "$LOCK_DIR" 2>/dev/null; then
		die "another CellBridge setup is running; if not, remove $LOCK_DIR and retry"
	fi
	trap 'rmdir "$LOCK_DIR" 2>/dev/null || true' 0
	trap 'exit 130' INT
	trap 'exit 143' TERM
}

register() {
	key=${1:-}
	priority=${2:-}
	[ -n "$key" ] || die "register needs a phone public key"
	[ -n "$priority" ] || die "register needs a priority between 1 and 10"
	case "$priority" in *[!0-9]*) die "priority must be a number between 1 and 10" ;; esac
	[ "$priority" -ge 1 ] && [ "$priority" -le 10 ] || die "priority must be between 1 and 10"
	acquire_lock
	check_internet || exit 75
	[ -f "$SETUP" ] || die "cellbridge-setup.sh must be beside this script"
	[ -f "$MONITOR" ] || die "cellbridge-failover.sh must be beside this script"
	# Not exec: the EXIT trap must outlive the setup call to release the lock.
	sh "$SETUP" --register --app-output \
		--phone-public-key "$key" --priority "$priority"
}

# `revert` is intentionally not offered here; recovery stays a deliberate,
# PC-driven action so an in-app mistake cannot silently tear down routing.
case "${1:-}" in
	check-internet) check_internet ;;
	register) shift; register "$@" ;;
	-h|--help|"")
		printf 'Usage: %s [check-internet|register KEY PRIORITY]\n' "$0" >&2
		[ "${1:-}" = "-h" ] || [ "${1:-}" = "--help" ] || exit 1
		;;
	*) printf 'Usage: %s [check-internet|register KEY PRIORITY]\n' "$0" >&2; exit 1 ;;
esac
