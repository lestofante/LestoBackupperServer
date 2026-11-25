#!/usr/bin/env bash
# Builds the server and the web client here, uploads them to <host> over ssh
# and restarts the systemd user service there. <host> is anything ssh
# accepts: user@machine, or a name from ~/.ssh/config.
#
#   deploy/deploy.sh myserver
#
# The server is built here for the server's CPU, whatever this machine's
# is, as a static binary (musl): it runs on any Linux there, whatever its C
# library. Cross-building needs cargo-zigbuild (zig is the C compiler and
# linker for the C parts: TLS's ring, the bundled SQLite); the script says
# what's missing. One-time setup on the server: see "Deploying" in the README.
set -euo pipefail

host=${1:?usage: deploy/deploy.sh <ssh host>}
dir=lestobackupper # on the server, in the home directory
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

# Runs a script on the server with sh, whatever the login shell there is
# (ssh hands commands to it: fish, for one, doesn't read sh syntax). The
# script goes inside single quotes, so it must not contain any.
remote() {
    case "$1" in *"'"*) echo "deploy.sh bug: a remote script contains a single quote" >&2; exit 2 ;; esac
    ssh "$host" "sh -c '$1'"
}

# The server's CPU decides what to build.
arch=$(remote "uname -m")
case "$arch" in
    x86_64 | amd64) target=x86_64-unknown-linux-musl ;;
    aarch64 | arm64) target=aarch64-unknown-linux-musl ;;
    armv7* | armv8l) target=armv7-unknown-linux-musleabihf ;;
    armv6*) target=arm-unknown-linux-musleabihf ;;
    riscv64) target=riscv64gc-unknown-linux-musl ;;
    *)
        echo "$host is $arch: no build target known for it here" >&2
        exit 1
        ;;
esac

# Everything needed to build that, said all at once.
missing=()
if ! command -v rustup >/dev/null; then
    missing+=("Rust through rustup (https://rustup.rs), which can add build targets")
else
    installed=$(rustup target list --installed)
    grep -qx "$target" <<<"$installed" || missing+=("rustup target add $target    # the server's CPU ($arch)")
    grep -qx wasm32-unknown-unknown <<<"$installed" || missing+=("rustup target add wasm32-unknown-unknown    # the web client")
fi
command -v zig >/dev/null || missing+=("zig: sudo pacman -S zig (or https://ziglang.org/download)")
cargo zigbuild --help >/dev/null 2>&1 || missing+=("cargo install --locked cargo-zigbuild")
if [ ${#missing[@]} -gt 0 ]; then
    echo "To build for $host ($arch, Rust target $target), install first:" >&2
    printf '  %s\n' "${missing[@]}" >&2
    exit 1
fi

echo "== building for $host ($target)"
cargo zigbuild --release --target "$target" --bin lestobackupper-server
cargo xtask web
binary=target/$target/release

echo "== uploading to $host:~/$dir"
# One tar stream over ssh: the binary and the web client, swapped in with
# renames so a running server keeps its files until it restarts.
tar -czf - -C "$binary" lestobackupper-server -C "$root/web" index.html style.css pkg \
    | remote "set -e
        mkdir -p ~/$dir/data ~/$dir/.upload
        rm -rf ~/$dir/.upload/*
        tar -xzf - -C ~/$dir/.upload
        mv -f ~/$dir/.upload/lestobackupper-server ~/$dir/lestobackupper-server
        mkdir -p ~/$dir/.upload/web
        mv ~/$dir/.upload/index.html ~/$dir/.upload/style.css ~/$dir/.upload/pkg ~/$dir/.upload/web/
        rm -rf ~/$dir/web.old
        if [ -d ~/$dir/web ]; then mv ~/$dir/web ~/$dir/web.old; fi
        mv ~/$dir/.upload/web ~/$dir/web
        rm -rf ~/$dir/web.old ~/$dir/.upload"

echo "== restarting"
remote "set -e
    if ! systemctl --user cat lestobackupper >/dev/null 2>&1; then
        echo \"no lestobackupper user service on this machine yet: see Deploying in the README\" >&2
        exit 1
    fi
    systemctl --user restart lestobackupper
    sleep 2
    systemctl --user --no-pager --lines=0 status lestobackupper | head -3
    journalctl --user -u lestobackupper --since -1min --no-pager -o cat | grep -E \"fingerprint|listening|web client|ERROR|WARN\" | tail -6 || true"
