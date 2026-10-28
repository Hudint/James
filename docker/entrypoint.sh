#!/bin/sh
# Checks the data directory, then starts James.
set -e
data="${JAMES_DATA_DIR:-/data}"
if [ ! -w "$data" ]; then
    echo "James (user $(id -u)) cannot write to $data. Give it the directory, e.g. on the host:" >&2
    echo "  sudo chown -R $(id -u):$(id -g) ./data" >&2
    exit 1
fi
exec /opt/james/bin/james "$@"
