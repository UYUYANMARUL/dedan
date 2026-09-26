#!/bin/sh
# lego "exec" DNS-01 provider. In the devnet the Rust nameserver (crates/resolver) stands in for Dedan resolvers
# serving control-plane-signed _acme-challenge TXT records; its management API has the pebble-challtestsrv shape.
#   present <fqdn> <txt-value>   publish the record
#   cleanup <fqdn> <txt-value>   remove it
set -eu
action="$1"
fqdn="$2"
value="${3:-}"
case "$action" in
  present) wget -qO- --post-data "{\"host\":\"$fqdn\",\"value\":\"$value\"}" http://resolver:8055/set-txt ;;
  cleanup) wget -qO- --post-data "{\"host\":\"$fqdn\"}" http://resolver:8055/clear-txt ;;
  *) echo "unknown action $action" >&2; exit 1 ;;
esac
