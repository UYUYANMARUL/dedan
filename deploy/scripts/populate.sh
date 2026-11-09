#!/bin/sh
# Fills the running devnet (demo only): node cities across Japan, then clients, each verified as a human (World ID
# mock), then every human attests the members of its own cell. Clients sit four or five to a cell:
#   sh deploy/scripts/populate.sh
# Built on add-node.sh and add-client.sh, so `make devnet-down` removes what it adds. Labels already on the devnet
# are skipped, so it can run again. Added node cities are capped at five (keys 30-44), clients at 95.
set -eu
cd "$(dirname "$0")/../.."
. deploy/scripts/lib.sh

NODES="osaka 34.6937 135.5023  kyoto 35.0116 135.7681  nagoya 35.1815 136.9066  fukuoka 33.5904 130.4017"
# four or five humans per cell, so every cell's median has several votes behind it (resolution-2 cells checked
# with the nameserver's /cell): Kansai 822e67f holds the osaka, kyoto and nagoya nodes; then Tokyo, Fukuoka and the
# cells of the frankfurt, seoul, newyork and istanbul nodes; the last twenty sit in cells nodes joined but do not
# live in (munich: frankfurt; busan: seoul and fukuoka; washington: newyork; sofia: istanbul; kobe: the Kansai three)
CLIENTS="nara 34.6851 135.8048  sakai 34.5733 135.4830  uji 34.8844 135.7997  otsu 35.0045 135.8686  gifu 35.4233 136.7607
  yokohama 35.4437 139.6380  kawasaki 35.5308 139.7029  saitama 35.8617 139.6455  chiba 35.6073 140.1063  hachioji 35.6664 139.3160
  saga 33.2635 130.3009  karatsu 33.4500 129.9683  itoshima 33.5572 130.1953  kasuga 33.5328 130.4702
  mainz 49.9929 8.2473  wiesbaden 50.0782 8.2398  darmstadt 49.8728 8.6512  offenbach 50.0956 8.7761
  incheon 37.4563 126.7052  suwon 37.2636 127.0286  seongnam 37.4200 127.1265  goyang 37.6584 126.8320
  newark 40.7357 -74.1724  jersey-city 40.7178 -74.0431  yonkers 40.9312 -73.8988  stamford 41.0534 -73.5387
  kadikoy 40.9910 29.0270  uskudar 41.0214 29.0161  gebze 40.8028 29.4307  silivri 41.0739 28.2464
  munich 48.1351 11.5820  augsburg 48.3705 10.8978  ingolstadt 48.7665 11.4258  landshut 48.5372 12.1522
  busan 35.1796 129.0756  daegu 35.8714 128.6014  ulsan 35.5384 129.3114  changwon 35.2280 128.6811
  washington 38.9072 -77.0369  baltimore 39.2904 -76.6122  annapolis 38.9784 -76.4922  arlington 38.8816 -77.0910
  sofia 42.6977 23.3219  thessaloniki 40.6401 22.9444  plovdiv 42.1354 24.7453  pernik 42.6052 23.0378
  kobe 34.6901 135.1955  himeji 34.8151 134.6853  okayama 34.6551 133.9195  hiroshima 34.3853 132.4553"

# add <script> <label lat lon>...: one add-node.sh / add-client.sh per triple
add() {
  script=$1; shift
  while [ $# -ge 3 ]; do
    if [ -n "$(host_field "$1" 1)$(host_field "user-$1" 1)" ]; then echo "== $1 is already on the devnet, skipped"
    else sh "deploy/scripts/$script" "$1" "$2" "$3"; fi
    shift 3
  done
}
add add-node.sh $NODES
add add-client.sh $CLIENTS

echo "== the clients verify with World ID (mock) and become attesters"
for c in $(ls deploy/devnet/clients); do sh deploy/scripts/human.sh "$c" | tail -n 1; done

echo "== every human attests the members of its own cell (after a minute: one batch per human per cell per minute)"
sleep 60
for h in $(users); do attest_from "$h" || true; done
sleep 12   # one nameserver sync

echo "== www.images.com from every human's container"
for h in $(users); do printf '  %-12s %s\n' "$h" "$(user "$h" dig +short www.images.com | grep -v dedan-cdn | tr '\n' ' ')"; done
