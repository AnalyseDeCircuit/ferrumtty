#!/bin/sh

# This script verifies each network control in a fresh, disposable container.
set -eu

readonly SERVER_IMAGE="${FERRUMTTY_LAB_SERVER_IMAGE:-ferrumtty-lab/mosh-server:1.4.0-arm64}"
readonly IPV6_NETWORK="${FERRUMTTY_LAB_IPV6_NETWORK:-ferrumtty-ipv6}"
readonly IPV6_SUBNET="${FERRUMTTY_LAB_IPV6_SUBNET:-fd42:726f:616d::/64}"
readonly TEST_MTU="${FERRUMTTY_LAB_MTU:-1200}"
readonly TEST_SECONDARY_ADDRESS="${FERRUMTTY_LAB_SECONDARY_ADDRESS:-198.51.100.10/32}"

run_network_admin() {
    docker run --rm --cap-add NET_ADMIN --entrypoint /bin/sh "${SERVER_IMAGE}" -eu -c "$1"
}

run_network_admin 'tc qdisc show dev eth0'
run_network_admin 'tc qdisc add dev eth0 root netem delay 100ms; tc qdisc show dev eth0'
run_network_admin 'tc qdisc add dev eth0 root netem loss 5%; tc qdisc show dev eth0'
run_network_admin 'tc qdisc add dev eth0 root netem duplicate 5%; tc qdisc show dev eth0'
run_network_admin 'tc qdisc add dev eth0 root netem delay 20ms reorder 25% 50%; tc qdisc show dev eth0'
run_network_admin "ip link set dev eth0 mtu ${TEST_MTU}; ip -details link show dev eth0"
run_network_admin "ip address add ${TEST_SECONDARY_ADDRESS} dev eth0; ip -4 address show dev eth0"

docker network rm "${IPV6_NETWORK}" >/dev/null 2>&1 || true
docker network create --ipv6 --subnet "${IPV6_SUBNET}" "${IPV6_NETWORK}" >/dev/null
trap 'docker network rm "${IPV6_NETWORK}" >/dev/null 2>&1 || true' EXIT HUP INT TERM
docker run --rm --network "${IPV6_NETWORK}" --entrypoint /bin/sh "${SERVER_IMAGE}" \
    -eu -c 'ip -6 address show dev eth0; ip -6 route show'
