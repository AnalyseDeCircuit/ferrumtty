// SPDX-License-Identifier: GPL-3.0-only

use aes::Aes128;
use base64::Engine;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use ocb3::Ocb3;
use ocb3::aead::consts::{U8, U12, U15, U16};
use ocb3::aead::{Aead, KeyInit, generic_array::GenericArray};
use std::io::{self, Read};
use zeroize::{Zeroize, Zeroizing};

const SESSION_KEY_BYTES: usize = 16;
const PACKET_NONCE_BYTES: usize = 8;
const AUTHENTICATION_TAG_BYTES: usize = 16;

type OcbWithEightByteNonce = Ocb3<Aes128, U8, U16>;
type OcbWithTwelveByteNonce = Ocb3<Aes128, U12, U16>;
type OcbWithFifteenByteNonce = Ocb3<Aes128, U15, U16>;

pub(crate) fn run_from_stdin() -> Result<(), String> {
    let mut input = Zeroizing::new(String::new());
    io::stdin()
        .read_to_string(&mut input)
        .map_err(|error| format!("failed to read OCB probe input: {error}"))?;
    let mut lines = input.lines();
    let encoded_key = lines.next().ok_or("missing session key")?;
    let packet_hex = lines.next().ok_or("missing packet bytes")?;
    if lines.next().is_some() {
        return Err("unexpected extra OCB probe input".to_owned());
    }

    let mut key = Zeroizing::new(
        STANDARD_NO_PAD
            .decode(encoded_key)
            .map_err(|_| "session key is not unpadded base64".to_owned())?,
    );
    if key.len() != SESSION_KEY_BYTES {
        return Err("session key must decode to 16 bytes".to_owned());
    }
    let mut packet = Zeroizing::new(decode_hex(packet_hex)?);
    if packet.len() < PACKET_NONCE_BYTES + AUTHENTICATION_TAG_BYTES {
        return Err("packet is too short for nonce prefix and authentication tag".to_owned());
    }

    let packet_nonce: [u8; PACKET_NONCE_BYTES] = packet[..PACKET_NONCE_BYTES]
        .try_into()
        .map_err(|_| "failed to read packet nonce".to_owned())?;
    let sealed_payload = &packet[PACKET_NONCE_BYTES..];
    let mut successes = 0_u8;

    successes += probe_eight_byte_nonce(&key, packet_nonce, sealed_payload);
    successes += probe_twelve_byte_nonce(&key, packet_nonce, sealed_payload);
    successes += probe_fifteen_byte_nonce(&key, packet_nonce, sealed_payload);

    key.zeroize();
    packet.zeroize();

    match successes {
        0 => Err("no tested RFC 7253 nonce mapping authenticated".to_owned()),
        1 => Ok(()),
        _ => Err("multiple nonce mappings authenticated unexpectedly".to_owned()),
    }
}

fn probe_eight_byte_nonce(key: &[u8], packet_nonce: [u8; 8], sealed_payload: &[u8]) -> u8 {
    let cipher = OcbWithEightByteNonce::new(GenericArray::from_slice(key));
    report_result(
        "packet-prefix-8",
        cipher.decrypt(GenericArray::from_slice(&packet_nonce), sealed_payload),
    )
}

fn probe_twelve_byte_nonce(key: &[u8], packet_nonce: [u8; 8], sealed_payload: &[u8]) -> u8 {
    let cipher = OcbWithTwelveByteNonce::new(GenericArray::from_slice(key));

    let mut zero_prefix = [0_u8; 12];
    zero_prefix[4..].copy_from_slice(&packet_nonce);
    let first_result = cipher.decrypt(GenericArray::from_slice(&zero_prefix), sealed_payload);
    let first_success = report_result("zero-prefix-12", first_result);

    let mut zero_suffix = [0_u8; 12];
    zero_suffix[..8].copy_from_slice(&packet_nonce);
    let second_result = cipher.decrypt(GenericArray::from_slice(&zero_suffix), sealed_payload);
    first_success + report_result("zero-suffix-12", second_result)
}

fn probe_fifteen_byte_nonce(key: &[u8], packet_nonce: [u8; 8], sealed_payload: &[u8]) -> u8 {
    let cipher = OcbWithFifteenByteNonce::new(GenericArray::from_slice(key));
    let mut zero_prefix = [0_u8; 15];
    zero_prefix[7..].copy_from_slice(&packet_nonce);
    report_result(
        "zero-prefix-15",
        cipher.decrypt(GenericArray::from_slice(&zero_prefix), sealed_payload),
    )
}

fn report_result(mapping: &str, result: Result<Vec<u8>, ocb3::aead::Error>) -> u8 {
    let Ok(mut plaintext) = result else {
        return 0;
    };

    println!(
        "authenticated nonce_mapping={mapping} plaintext_hex={}",
        encode_hex(&plaintext)
    );
    plaintext.zeroize();
    1
}

pub(crate) fn decode_hex(value: &str) -> Result<Vec<u8>, String> {
    let mut pairs = value.as_bytes().chunks_exact(2);
    if !pairs.remainder().is_empty() {
        return Err("packet hex must contain complete bytes".to_owned());
    }

    pairs
        .by_ref()
        .map(|pair| {
            let high = decode_nibble(pair[0])?;
            let low = decode_nibble(pair[1])?;
            Ok((high << 4) | low)
        })
        .collect()
}

fn decode_nibble(value: u8) -> Result<u8, String> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err("packet contains a non-hex character".to_owned()),
    }
}

pub(crate) fn encode_hex(bytes: &[u8]) -> String {
    const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        output.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{decode_hex, encode_hex};

    #[test]
    fn hex_codec_round_trips_bytes() {
        let bytes = [0x00, 0x01, 0xab, 0xff];

        assert_eq!(decode_hex(&encode_hex(&bytes)), Ok(bytes.to_vec()));
    }

    #[test]
    fn hex_decoder_rejects_partial_byte() {
        assert_eq!(
            decode_hex("0"),
            Err("packet hex must contain complete bytes".to_owned())
        );
    }
}
