#!/bin/sh
# Locate an existing Node runtime without installing software or changing PATH profiles.
set -eu
repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
node_bin=$(command -v node || true)
if [ -z "$node_bin" ] && [ -x "$HOME/.local/share/pi-node/current/bin/node" ]; then
  node_bin="$HOME/.local/share/pi-node/current/bin/node"
fi
if [ -z "$node_bin" ]; then
  printf '%s\n' 'Node.js 20+ is required. Install Node or use the Node runtime supplied by your Pi installation.' >&2
  exit 1
fi
exec "$node_bin" "$repo_dir/scripts/start-tailnet.mjs" "$@"
