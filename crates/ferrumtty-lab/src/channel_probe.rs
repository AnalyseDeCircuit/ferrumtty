// SPDX-License-Identifier: GPL-3.0-only

use crate::ocb_probe::decode_hex;
use crate::protobuf_probe::inspect_transport_plaintext;
use ferrumtty_crypto::{PeerRole, SecureChannel, SessionKey};
use ferrumtty_wire::{Fragment, decode_compressed_update};
use std::io::{self, Read};
use zeroize::Zeroizing;

/// Verifies an observed client packet through the production envelope API.
pub(crate) fn run_from_stdin(receiver_role: PeerRole) -> Result<(), String> {
    let authenticated = read_and_open(receiver_role)?;

    println!(
        "production channel authenticated counter={} plaintext_bytes={}",
        authenticated.counter,
        authenticated.plaintext.len()
    );
    inspect_transport_plaintext(&authenticated.plaintext)?;
    let fragment = Fragment::parse(&authenticated.plaintext)
        .map_err(|_| "production fragment parser rejected observed plaintext".to_owned())?;
    if fragment.header.index == 0 && fragment.header.is_final {
        let update = decode_compressed_update(&fragment.body)
            .map_err(|_| "production message decoder rejected observed state".to_owned())?;
        let instructions = update
            .decode_instructions()
            .map_err(|_| "production instruction decoder rejected observed delta".to_owned())?;
        println!(
            "production wire decoded base_state={} target_state={} acknowledged_state={} discard_before={} instructions={} padding_bytes={}",
            update.base_state,
            update.target_state,
            update.acknowledged_state,
            update.discard_before,
            instructions.instructions.len(),
            update.padding.len()
        );
    }
    Ok(())
}

/// Prints only fragment metadata so large synthetic payloads stay out of logs.
pub(crate) fn run_fragment_from_stdin(receiver_role: PeerRole) -> Result<(), String> {
    const TRANSPORT_PREFIX_BYTES: usize = 14;
    let authenticated = read_and_open(receiver_role)?;
    let (transport_prefix, fragment_body) = authenticated
        .plaintext
        .split_at_checked(TRANSPORT_PREFIX_BYTES)
        .ok_or("authenticated fragment is shorter than its prefix")?;

    println!(
        "production channel authenticated counter={} transport_prefix_bytes={} fragment_body_bytes={}",
        authenticated.counter,
        transport_prefix.len(),
        fragment_body.len()
    );
    Ok(())
}

fn read_and_open(receiver_role: PeerRole) -> Result<ferrumtty_crypto::AuthenticatedPacket, String> {
    let mut input = Zeroizing::new(String::new());
    io::stdin()
        .read_to_string(&mut input)
        .map_err(|error| format!("failed to read channel probe input: {error}"))?;
    let mut lines = input.lines();
    let encoded_key = lines.next().ok_or("missing session key")?;
    let packet_hex = lines.next().ok_or("missing packet bytes")?;
    if lines.next().is_some() {
        return Err("unexpected extra channel probe input".to_owned());
    }

    let key = SessionKey::decode(encoded_key).map_err(|_| "invalid session key".to_owned())?;
    let packet = Zeroizing::new(decode_hex(packet_hex)?);
    let channel = SecureChannel::new(receiver_role, key);
    let authenticated = channel
        .open(&packet)
        .map_err(|_| "production channel rejected observed packet".to_owned())?;

    Ok(authenticated)
}
