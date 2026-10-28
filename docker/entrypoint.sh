#!/bin/sh
# Prepares the data directory, then starts James as the unprivileged user `james`.
set -e
data="${JAMES_DATA_DIR:-/data}"
if [ "$(id -u)" = "0" ]; then
    mkdir -p "$data"
    if [ "$(stat -c %u "$data")" != "1001" ]; then
        # A directory Docker created for the bind mount belongs to root and is empty: give it to
        # james. Existing files are left alone (they may be anyone's); then the owner decides.
        # Unreadable (another user's private directory) counts as not empty.
        entries=$(ls -A "$data" 2>/dev/null) || entries=unreadable
        if [ -n "$entries" ]; then
            echo "$data belongs to another user and is not empty. Give it to James (uid 1001)," >&2
            echo "e.g. on the host: sudo chown -R 1001:1001 ./data" >&2
            exit 1
        fi
        echo "Giving $data to the james user (uid 1001)" >&2
        chown -h 1001:1001 "$data"
    fi
    # Drop to james for good: no capabilities, no way to regain any.
    exec env HOME=/home/james setpriv --reuid=1001 --regid=1001 --init-groups \
        --inh-caps=-all --bounding-set=-all --no-new-privs /opt/james/bin/james "$@"
fi
# Started as another user (compose `user:`): that user must own the directory.
if [ ! -w "$data" ]; then
    echo "James (user $(id -u)) cannot write to $data. Give it the directory, e.g. on the host:" >&2
    echo "  sudo chown -R $(id -u):$(id -g) ./data" >&2
    exit 1
fi
exec /opt/james/bin/james "$@"
