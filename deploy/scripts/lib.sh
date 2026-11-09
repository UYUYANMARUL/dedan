# Shared helpers for the devnet scripts (sourced, POSIX sh). Containers are addressed by name, so cities added
# while running (add-node.sh starts plain docker containers, not compose services) work like the static five.
COMPOSE="docker compose --progress quiet -f deploy/docker-compose.yml"
MNEMONIC="test test test test test test test test test test test junk"
STATIC_HOSTS=deploy/wan/hosts.txt
DYNAMIC_HOSTS=deploy/devnet/dynamic-hosts.txt   # written by add-node.sh, read by the nameserver too
NS_API=${NS_API:-http://127.0.0.1:8055}          # the nameserver's management API (host port, or resolver:8055 inside)

hosts() { cat "$STATIC_HOSTS" "$DYNAMIC_HOSTS" 2>/dev/null | grep -v '^#'; }
cities() { hosts | awk '$4=="node" {print $2}'; }                       # every city with a CDN node
users() { hosts | awk '$4=="user" {sub(/^user-/, "", $1); print $1}'; }  # every human: city users and added clients
host_field() { hosts | awk -v n="$1" -v c="$2" '$1==n {print $c; exit}'; }
node_ip() { host_field "$1" 3; }
user_ip() { host_field "user-$1" 3; }
# a field of a node's or a client's record (deploy/devnet/nodes.json for the static cities, one file per added
# node city or client: label, cell, attester key index, ...)
node_field() { python3 -c "
import glob, json, sys
for f in ['deploy/devnet/nodes.json'] + sorted(glob.glob('deploy/devnet/dynamic/*/node.json')) + sorted(glob.glob('deploy/devnet/clients/*/client.json')):
    for n in json.load(open(f))['nodes']:
        if n['label'] == '$1': print(n['$2']); sys.exit()
sys.exit('no node $1')"; }
registry() { python3 -c "import json; print(json.load(open('contracts/deployments/devnet.json'))['dedanRegistry'])"; }
deployed() { python3 -c "import json; print(json.load(open('contracts/deployments/devnet.json'))['$1'])"; }
cells_contract() { deployed dedanCells; }
# the nodes that joined a cell (from the nameserver's mirror of the chain), space-separated
cell_members() { curl -s "$NS_API/cells" | python3 -c "
import json,sys
for c in json.load(sys.stdin)['cells']:
    if c['cell'] == '$1': print(' '.join(c['members']))"; }
# the H3 resolution-2 cell of a point, and with a third argument the cells within k rings (JSON array)
cell_of() { curl -sf "$NS_API/cell?lat=$1&lon=$2&k=${3:-0}" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d['cell'] if '${3:-}' == '' else json.dumps([int(c) for c in d['cells']]))"; }
# an ENS text record read the way any client does it: the Universal Resolver walks the tree from the root
# (`resolve(dnsEncodedName, text(namehash, key))`) and answers from the name's resolver
ens_text() {
  dns=$(python3 -c "print('0x' + ''.join('%02x%s' % (len(l), l.encode().hex()) for l in '$1'.split('.')) + '00')")
  $COMPOSE run --rm -T deployer "node=\$(cast namehash $1); data=\$(cast calldata 'text(bytes32,string)' \$node '$2'); out=\$(cast call $(deployed universalResolver) 'resolve(bytes,bytes)(bytes,address)' $dns \$data --rpc-url http://anvil:8545 | head -1); cast abi-decode 'f()(string)' \$out" 2>/dev/null | tr -d '\r\n"'
}

user() { c=$1; shift; docker exec -i "dedan-devnet-user-$c-1" "$@"; }
node() { c=$1; shift; docker exec -i "dedan-devnet-$c-1" "$@"; }
cast() { $COMPOSE run --rm -T deployer "cast $*"; }
private_key() { cast wallet private-key "\"$MNEMONIC\"" --mnemonic-index "$1" | tr -d '\r\n'; }
address_of() { cast wallet address --mnemonic "\"$MNEMONIC\"" --mnemonic-index "$1" | tr -d '\r\n'; }
is_attester() { cast call "$(registry)" "\"isAttester(address)(bool)\"" "$1" --rpc-url http://anvil:8545 | tr -d '\r\n'; }
# World ID mock (the devnet's scripted humans; real humans go through WORLD-ID.md): a Proof of Human (schema 1)
# proof with any non-zero nullifier passes; the nullifier is burned per action, so one per human
register_attester() { cast send "$(registry)" "\"registerAttester((uint256,uint256,uint64,uint64,uint256,uint256[5]))\"" "\"($2,0,0,1,0,[0,0,0,0,0])\"" --private-key "$(private_key "$1")" --rpc-url http://anvil:8545 >/dev/null; }
hex() { printf '0x%s' "$(printf '%s' "$1" | xxd -p | tr -d '\n')"; }
# minimum round trip in ms from container $1 (a city or user-<city>) to ip $2, as an attestation keeps it (jitter only
# ever adds delay, so the fastest of three is the closest to the path); 9999 when unreachable
ping_rtt() {
  r=$(docker exec -i "dedan-devnet-$1-1" ping -c 3 -i 0.2 -q "$2" 2>/dev/null | awk -F' = ' '/round-trip|rtt/ {split($2, t, "/"); printf "%.0f", t[1]}')
  printf '%s' "${r:-9999}"
}
# the X-Dedan-Node token the node at $2 mints for the user in city $1; one retry, since a transient miss must not
# turn into a malformed batch (an empty token still attests, and the map rejects it as bad_token)
fetch_token() {
  for _ in 1 2; do
    t=$(user "$1" curl -s -m 5 -o /dev/null -D - -H 'Host: www.images.com' "http://$2/assets/style.css" | awk -F': ' 'tolower($1)=="x-dedan-node" {print $2}' | tr -d '\r')
    [ -n "$t" ] && break
  done
  printf '%s' "$t"
}
# the human in city $1 (its registered attester, or the key ATTESTER_INDEX) pings and fetches from every node
# that joined its cell (or those of them named after it), then sends one attest batch to the cells contract;
# VERBOSE=1 prints every measurement and the transaction
attest_from() {
  c=$1; shift
  cell=$(node_field "$c" cell); prefix=$(user_ip "$c" | awk -F. '{printf "%s.%s.%s.0/24", $1, $2, $3}')
  members=$(cell_members "$cell")
  if [ $# -gt 0 ]; then targets=""; for n in "$@"; do case " $members " in *" $n "*) targets="$targets $n";; esac; done; else targets=$members; fi
  [ -n "$(echo $targets)" ] || { echo "$c: no node has joined cell $cell (members: ${members:-none}); nothing to attest"; return 1; }
  items=""; count=0
  for n in $targets; do
    ip=$(node_ip "$n"); rtt=$(ping_rtt "user-$c" "$ip"); tok=$(fetch_token "$c" "$ip")
    [ -z "${VERBOSE:-}" ] || echo "  $n ($ip): $rtt ms, token ${tok:-none}"
    items="$items$([ -n "$items" ] && echo ,)($(cast keccak "$n" | tr -d '\r\n'),$rtt,0,$(hex "$tok"),$(hex "$prefix"))"; count=$((count + 1))
  done
  if ! out=$(cast send "$(cells_contract)" "\"attest(uint64,(bytes32,uint32,uint32,bytes,bytes)[])\"" "$cell" "\"[$items]\"" \
      --private-key "$(private_key "${ATTESTER_INDEX:-$(node_field "$c" attester)}")" --rpc-url http://anvil:8545 2>&1); then
    echo "attest from $c rejected by the contract:"; echo "$out" | grep -iE "error|revert|EAC" | head -3
    echo "$out" | grep -q "0xcab98054\|NotAttester" && echo "(NotAttester: the key holds no ROLE_ATTESTER, so this human is not World ID-verified; verify first)"
    echo "$out" | grep -q "0x6fed7d85\|TooSoon" && echo "(TooSoon: a human may send one batch per cell per minute)"
    echo "$out" | grep -q "0x291fc442\|NotMember" && echo "(NotMember: a node that did not join this cell cannot be attested in it)"
    return 1
  fi
  echo "$c: attested $count round trips from $prefix, one vote each in cell $cell$([ -z "${VERBOSE:-}" ] || echo "$out" | awk '$1=="transactionHash" {printf ", tx %s", $2}')"
}
