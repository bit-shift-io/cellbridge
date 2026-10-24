#!/usr/bin/env bash
# Start CellBridge's rootless Linux WireGuard test peer.
set -euo pipefail
umask 077

usage() {
    printf '%s\n' \
        'Usage: ./linux-wg-peer.sh [--peer PHONE_PUBLIC_KEY] [--listen IP:PORT] [--key-file PATH]' \
        '' \
        'First run: pass the phone public key shown in Configuration, or enter it when prompted.' \
        'Later runs reuse the saved phone public key and Linux private key.' \
        'The listen address defaults to the IPv4 source address of the default route, port 51820.' \
        'Use --listen to select a different LAN interface/address.' \
        '' \
        'Requires Bash, Cargo/Rust, and iproute2 (unless --listen is supplied).' \
        'Runs in the foreground. Keep the terminal open; Ctrl-C or quit stops the peer.' \
        'No sudo, kernel WireGuard interface, route, or firewall changes.'
}

die() { printf 'Error: %s\n' "$1" >&2; exit 1; }

root=$(dirname -- "$(readlink -f -- "${BASH_SOURCE[0]}")")
state_dir=${XDG_STATE_HOME:-"${HOME:?HOME must be set}/.local/state"}/cellbridge
key_file=$state_dir/test-router.key
phone_file=$state_dir/phone-public.key
peer=''
listen=''

while (($#)); do
    case "$1" in
        --help|-h) usage; exit 0 ;;
        --peer|--listen|--key-file)
            (($# >= 2)) && [[ -n $2 ]] || die "Missing value for $1"
            case "$1" in
                --peer) peer=$2 ;;
                --listen) listen=$2 ;;
                --key-file) key_file=$2 ;;
            esac
            shift 2 ;;
        *) die "Unknown argument: $1 (use --help)" ;;
    esac
done

command -v cargo >/dev/null || die 'Install Rust/Cargo before running this script.'

if [[ -z $peer && -f $phone_file ]]; then
    IFS= read -r peer < "$phone_file" || die 'Cannot read saved phone public key.'
fi
if [[ -z $peer ]]; then
    [[ -t 0 ]] || die 'Provide --peer PHONE_PUBLIC_KEY on the first run.'
    read -r -p 'Phone public key (from Configuration): ' peer
fi
[[ $peer =~ ^[A-Za-z0-9+/]{43}=$ ]] || die 'Phone public key must be a Base64-encoded 32-byte WireGuard public key.'

if [[ -z $listen ]]; then
    command -v ip >/dev/null || die 'Install iproute2 or supply --listen LAN_IP:51820.'
    # Route lookup only: sends no packets and needs no internet connectivity.
    route=$(ip -4 route get 1.1.1.1) || die 'Cannot determine LAN address; supply --listen LAN_IP:51820.'
    read -r -a fields <<< "$route"
    for ((i=0; i<${#fields[@]}-1; i++)); do
        if [[ ${fields[i]} == src ]]; then
            listen=${fields[i+1]}:51820
            break
        fi
    done
    [[ -n $listen ]] || die 'Cannot determine LAN address; supply --listen LAN_IP:51820.'
fi

printf 'Building Linux WireGuard test peer…\n'
cargo build --quiet --manifest-path "$root/rust/Cargo.toml" \
    --target-dir "$root/rust/target" --bin standin_router
binary=$root/rust/target/debug/standin_router

mkdir -p -- "$state_dir"
if [[ ! -e $key_file && ! -L $key_file ]]; then
    [[ -d $(dirname -- "$key_file") ]] || die 'The key-file parent directory must already exist.'
    "$binary" init --key-file "$key_file"
fi
# Only the public phone identity is saved here. Never source this file as shell code.
printf '%s\n' "$peer" > "$phone_file"

printf '\nPhone Configuration:\n  Address: 10.10.10.2/24\n  Endpoint: %s\n  MTU: 1420\n  Preshared key: empty\n' "$listen"
printf '  Peer public key: use the Public key printed below.\n\n'
printf 'Tap Test Wi-Fi connection in Configuration. No SIM or internet is required.\n'
printf 'If UDP bind fails, check that another peer is not already using this address/port.\n\n'
exec "$binary" serve --key-file "$key_file" --peer "$peer" --listen "$listen"
