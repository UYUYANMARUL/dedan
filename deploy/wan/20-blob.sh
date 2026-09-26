#!/bin/sh
# The class-probe blob every node serves at /.well-known/dedan/blob/<n> (n MB); 50 MB on the devnet.
set -eu
[ -n "${NODE_ID:-}" ] || exit 0
mkdir -p /usr/share/nginx/blob
[ -f /usr/share/nginx/blob/50 ] || head -c 52428800 /dev/urandom > /usr/share/nginx/blob/50
