#!/bin/sh

# This script passes an observed key and packet only through an in-memory pipe.
set -eu

readonly SERVER_IMAGE="${FERRUMTTY_LAB_SERVER_IMAGE:-ferrumtty-lab/mosh-server:1.4.0-arm64}"
readonly SERVER_PORT="${FERRUMTTY_LAB_SERVER_PORT:-60001}"
readonly CLIENT_PACKET_INDEX="${FERRUMTTY_LAB_CLIENT_PACKET_INDEX:-2}"

docker run --rm -i --cap-add NET_RAW \
    --env FERRUMTTY_LAB_SERVER_PORT="${SERVER_PORT}" \
    --env FERRUMTTY_LAB_CLIENT_PACKET_INDEX="${CLIENT_PACKET_INDEX}" \
    --entrypoint /bin/sh "${SERVER_IMAGE}" -seu <<'LAB_SCRIPT' \
    | cargo run --quiet --package ferrumtty-lab -- verify-client-packet
readonly server_port=${FERRUMTTY_LAB_SERVER_PORT}
readonly client_packet_index=${FERRUMTTY_LAB_CLIENT_PACKET_INDEX}
readonly capture_file=/tmp/ferrumtty-packet.pcap
readonly client_capture_file=/tmp/ferrumtty-client-packet.pcap
readonly network_headers_bytes=48
readonly pcap_global_header_bytes=24
readonly pcap_record_header_bytes=16

tcpdump -i any -U -w "${capture_file}" udp port "${server_port}" >/dev/null 2>&1 &
capture_pid=$!
sleep 1
startup_output=$(/usr/bin/mosh-server new -p "${server_port}" -- /bin/sh -lc \
    'printf "FERRUMTTY-SYNTHETIC\n"; sleep 5' 2>/dev/null)
connect_fields=$(printf '%s\n' "${startup_output}" \
    | awk '/^MOSH CONNECT / { print $3, $4; exit }')
unset startup_output
set -- ${connect_fields}
unset connect_fields
session_port=$1
session_key=$2

printf 'exit\n' \
    | MOSH_KEY="${session_key}" COLUMNS=80 LINES=24 timeout 8 script -q -e \
        -c "stty rows 24 cols 80; exec /usr/bin/mosh-client 127.0.0.1 ${session_port}" \
        /dev/null >/dev/null 2>&1
sleep 1
kill "${capture_pid}" 2>/dev/null || true
wait "${capture_pid}" 2>/dev/null || true

tcpdump -r "${capture_file}" -w "${client_capture_file}" \
    "dst port ${server_port}" 2>/dev/null
case ${client_packet_index} in
    ''|*[!0-9]*|0) printf 'invalid client packet index\n' >&2; exit 2 ;;
esac
record_offset=${pcap_global_header_bytes}
current_index=1
while [ "${current_index}" -lt "${client_packet_index}" ]; do
    frame_bytes=$(od -An -tu4 -j $((record_offset + 8)) -N 4 \
        "${client_capture_file}" | tr -d ' ')
    record_offset=$((record_offset + pcap_record_header_bytes + frame_bytes))
    current_index=$((current_index + 1))
done
selected_frame_bytes=$(od -An -tu4 -j $((record_offset + 8)) -N 4 \
    "${client_capture_file}" | tr -d ' ')
payload_bytes=$((selected_frame_bytes - network_headers_bytes))
payload_offset=$((record_offset + pcap_record_header_bytes + network_headers_bytes))
payload_hex=$(dd if="${client_capture_file}" bs=1 skip="${payload_offset}" \
    count="${payload_bytes}" 2>/dev/null | od -An -v -tx1 | tr -d ' \n')

printf '%s\n%s\n' "${session_key}" "${payload_hex}"
unset session_key payload_hex
LAB_SCRIPT
