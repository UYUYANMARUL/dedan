#!/bin/sh
# nginx entrypoint hook of the `origin-tampered` service (`make demo-settlement`): the real origin's site,
# copied from its read-only mount, with one byte appended to every file. Bytes served from here match neither
# the current nor the previous digest of the real origin, which is what a node serving wrong bytes looks like.
set -e
cp -r /srv/origin/. /usr/share/nginx/html/
find /usr/share/nginx/html -type f -exec sh -c 'printf "\n" >> "$1"' _ {} \;
echo "origin-tampered: $(find /usr/share/nginx/html -type f | wc -l) files tampered"
