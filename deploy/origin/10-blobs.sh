#!/bin/sh
# nginx entrypoint hook: generate the large sample assets used for throughput probes (not committed to git).
set -e
d=/usr/share/nginx/html/assets
mkdir -p "$d"
[ -f "$d/blob-2m.bin" ] || head -c 2097152 /dev/urandom > "$d/blob-2m.bin"
[ -f "$d/blob-16m.bin" ] || head -c 16777216 /dev/urandom > "$d/blob-16m.bin"
