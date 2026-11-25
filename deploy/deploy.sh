#!/usr/bin/env bash
# Builds the server and the web client here, uploads them to <host> over ssh
# and restarts the systemd user service there. <host> is anything ssh
# accepts: user@machine, or a name from ~/.ssh/config.
#
#   deploy/deploy.sh myserver
#
# One-time setup on the server: see "Deploying" in the README. Only Rust is
# needed here; the server needs systemd (and ffmpeg, for video thumbnails).
set -euo pipefail

host=${1:?usage: deploy/deploy.sh <ssh host>}
dir=lestobackupper # on the server, in the home directory
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

# The binary runs there as built here: same CPU architecture needed.
here=$(uname -m)
there=$(ssh "$host" uname -m)
if [ "$here" != "$there" ]; then
    echo "this machine is $here, $host is $there: build on the server instead (see README)" >&2
    exit 1
fi

echo "== building"
cargo build --release --bin lestobackupper-server
cargo xtask web

echo "== uploading to $host:~/$dir"
# One tar stream over ssh: the binary and the web client, swapped in with
# renames so a running server keeps its files until it restarts.
tar -czf - -C target/release lestobackupper-server -C "$root/web" index.html style.css pkg \
    | ssh "$host" "set -e
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
ssh "$host" "set -e
    if ! systemctl --user cat lestobackupper >/dev/null 2>&1; then
        echo 'no lestobackupper user service on this machine yet: see \"Deploying\" in the README' >&2
        exit 1
    fi
    systemctl --user restart lestobackupper
    sleep 2
    systemctl --user --no-pager --lines=0 status lestobackupper | head -3
    journalctl --user -u lestobackupper --since '-1min' --no-pager -o cat | grep -E 'fingerprint|listening|web client|ERROR|WARN' | tail -6 || true"
