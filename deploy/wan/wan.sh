#!/bin/sh
# Simulated WAN. Runs at container start (nginx entrypoint hook, or explicitly for non-nginx containers).
# Reads this container's CITY, then adds a one-way netem delay of RTT/2 toward every city's /20 on the
# 10.77.0.0/16 network, so the round trip between two containers matches deploy/wan/latency.txt.
# Same-city traffic gets a 2 ms round trip. One netem band per city (15 at most). Needs CAP_NET_ADMIN.
# latency.txt rows: `a b rtt_ms [jitter_ms]`.
set -eu
me="${CITY:?CITY env is required (frankfurt, tokyo, seoul, newyork, istanbul)}"
hosts=/wan/hosts.txt
matrix=/wan/latency.txt

ifc=$(ip -o -4 addr show | awk '/10\.77\./ {print $2; exit}')
[ -n "$ifc" ] || { echo "wan: no interface on 10.77.0.0/16, skipping" >&2; exit 0; }

# prio qdisc with 16 bands (the kernel's maximum): bands 1-15 carry one delayed city each, band 16 (index 15) is
# the undelayed default.
tc qdisc replace dev "$ifc" root handle 1: prio bands 16 priomap 15 15 15 15 15 15 15 15 15 15 15 15 15 15 15 15
band=1
# one line per city: its /20 is the third octet rounded down to a multiple of 16
awk '$1 !~ /^#/ && NF >= 4 { split($3, o, "."); printf "%s 10.77.%d.0/20\n", $2, int(o[3] / 16) * 16 }' "$hosts" | sort -u | while read -r city net; do
  if [ "$city" = "$me" ]; then
    rtt=2
  else
    entry=$(awk -v a="$me" -v b="$city" '($1==a && $2==b) || ($1==b && $2==a) {print $3, $4; exit}' "$matrix")
    rtt=${entry%% *}; jitter=${entry#* }; [ "$jitter" != "$entry" ] || jitter=""
  fi
  [ -n "$rtt" ] || { echo "wan: no latency entry for $me <-> $city, leaving $net undelayed" >&2; continue; }
  [ "$band" -le 15 ] || { echo "wan: more than 15 cities, leaving $net undelayed" >&2; continue; }
  half=$((rtt / 2))
  # an optional fourth column is a per-packet jitter in ms (uniform), used by clients added at runtime
  tc qdisc replace dev "$ifc" parent 1:$band handle $((band + 10)): netem delay ${half}ms ${jitter:+${jitter}ms} limit 100000
  tc filter add dev "$ifc" protocol ip parent 1:0 prio 1 u32 match ip dst "$net" flowid 1:$band
  echo "wan: $me -> $city ($net) one-way ${half} ms"
  band=$((band + 1))
done
