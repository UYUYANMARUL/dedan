#!/bin/sh
# Is the CDN router working, as a client sees it? From the user container in each city (the static five and any
# added with add-node.sh): resolve www.images.com through the nameserver the ordinary way (the container's DNS; no
# ECS, the source address decides), fetch an image by name twice, and record which node served it. Passes when at
# least two different nodes served the cities, every response came from the node that owns the connected IP (its
# token says so), and every body is byte-identical to the origin's. Needs a running devnet; `make demo` runs it last.
set -eu
cd "$(dirname "$0")/../.."
. deploy/scripts/lib.sh
asset=${ASSET:-/images/photo-1.png}

origin_sha=$(docker exec -i dedan-devnet-origin-1 sh -c "wget -qO- http://127.0.0.1$asset | sha256sum" | cut -c1-16)
echo "origin $asset sha256 $origin_sha"
printf '%-12s %-14s %-14s %-12s %-10s %-8s %s\n' city "dns answer" "connected to" node cache bytes sha256
ips=""; ok=1
for c in $(users); do
  answer=$(user "$c" dig +short www.images.com | grep -vE 'dedan-cdn' | head -1)
  out=$(user "$c" sh -c "curl -s -m 10 -D /tmp/h1 -o /dev/null http://www.images.com$asset;
    curl -s -m 10 -D /tmp/h2 -o /tmp/b -w '%{remote_ip} %{size_download}' http://www.images.com$asset; echo;
    sha256sum /tmp/b | cut -c1-16; cat /tmp/h1 /tmp/h2 | grep -iE '^(x-dedan-node|x-cache):' | tr -d '\r'")
  remote=$(echo "$out" | sed -n 1p | cut -d' ' -f1); bytes=$(echo "$out" | sed -n 1p | cut -d' ' -f2); sha=$(echo "$out" | sed -n 2p)
  node=$(echo "$out" | awk -F': ' 'tolower($1)=="x-dedan-node" {split($2,t,"."); print t[1]}' | tail -1)
  cache=$(echo "$out" | awk -F': ' 'tolower($1)=="x-cache" {print $2}' | paste -sd'>' -)
  printf '%-12s %-14s %-14s %-12s %-10s %-8s %s\n' "$c" "$answer" "$remote" "$node" "$cache" "$bytes" "$sha"
  ips="$ips $remote"
  owner=$(hosts | awk -v ip="$remote" '$3==ip && $4=="node" {print $1}')
  [ -n "$node" ] && [ "$owner" = "$node" ] || { echo "   ^ $c: connected to $remote, owned by '${owner:-nobody}', served by '${node:-no token}'"; ok=0; }
  [ "$sha" = "$origin_sha" ] || { echo "   ^ $c: body differs from the origin"; ok=0; }
done
distinct=$(echo "$ips" | tr ' ' '\n' | grep . | sort -u | wc -l | tr -d ' ')
echo "distinct nodes serving $(users | wc -l | tr -d ' ') clients: $distinct (need >= 2); same bytes as the origin everywhere: $([ "$ok" = 1 ] && echo yes || echo NO)"
[ "$distinct" -ge 2 ] || ok=0
if [ "$ok" = 1 ]; then echo "CDN ROUTING OK"; else echo "CDN ROUTING FAILED"; exit 1; fi
