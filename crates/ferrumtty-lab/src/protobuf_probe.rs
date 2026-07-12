// SPDX-License-Identifier: GPL-3.0-only

use flate2::read::ZlibDecoder;
use std::io::Read;

/// Separates the transport prefix from a zlib stream and lists generic fields.
pub(crate) fn inspect_transport_plaintext(plaintext: &[u8]) -> Result<(), String> {
    let compressed_offset = plaintext
        .windows(2)
        .position(|pair| is_zlib_header(pair[0], pair[1]))
        .ok_or("authenticated plaintext contains no zlib header")?;
    let mut decoder = ZlibDecoder::new(&plaintext[compressed_offset..]);
    let mut message = Vec::new();
    decoder
        .read_to_end(&mut message)
        .map_err(|error| format!("failed to decompress authenticated payload: {error}"))?;

    println!(
        "transport_prefix_bytes={} protobuf_bytes={}",
        compressed_offset,
        message.len()
    );
    for field in parse_fields(&message)? {
        match field.value {
            FieldValue::Varint(value) => {
                println!("protobuf_field={} wire=varint value={value}", field.number);
            }
            FieldValue::Bytes(value) => {
                println!(
                    "protobuf_field={} wire=bytes length={}",
                    field.number,
                    value.len()
                );
            }
        }
    }
    Ok(())
}

fn is_zlib_header(compression_method: u8, flags: u8) -> bool {
    compression_method & 0x0f == 8
        && (usize::from(compression_method) * 256_usize + usize::from(flags)) % 31 == 0
}

struct Field<'a> {
    number: u64,
    value: FieldValue<'a>,
}

enum FieldValue<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
}

fn parse_fields(mut message: &[u8]) -> Result<Vec<Field<'_>>, String> {
    let mut fields = Vec::new();
    while !message.is_empty() {
        let (key, key_bytes) = decode_varint(message)?;
        message = &message[key_bytes..];
        let number = key >> 3;
        let wire_type = key & 0x07;
        if number == 0 {
            return Err("protobuf field number must be nonzero".to_owned());
        }

        let value = match wire_type {
            0 => {
                let (value, consumed) = decode_varint(message)?;
                message = &message[consumed..];
                FieldValue::Varint(value)
            }
            2 => {
                let (length, consumed) = decode_varint(message)?;
                message = &message[consumed..];
                let length = usize::try_from(length)
                    .map_err(|_| "protobuf byte field length exceeds usize".to_owned())?;
                let (value, remainder) = message
                    .split_at_checked(length)
                    .ok_or("protobuf byte field is truncated")?;
                message = remainder;
                FieldValue::Bytes(value)
            }
            _ => return Err(format!("unsupported protobuf wire type {wire_type}")),
        };
        fields.push(Field { number, value });
    }
    Ok(fields)
}

fn decode_varint(bytes: &[u8]) -> Result<(u64, usize), String> {
    let mut value = 0_u64;
    for (index, &byte) in bytes.iter().take(10).enumerate() {
        let payload = u64::from(byte & 0x7f);
        if index == 9 && payload > 1 {
            return Err("protobuf varint overflows u64".to_owned());
        }
        value |= payload << (index * 7);
        if byte & 0x80 == 0 {
            return Ok((value, index + 1));
        }
    }
    Err("protobuf varint is truncated or too long".to_owned())
}

#[cfg(test)]
mod tests {
    use super::{FieldValue, decode_varint, parse_fields};

    #[test]
    fn parses_varint_and_bytes_fields() {
        let fields = parse_fields(&[0x08, 0x96, 0x01, 0x12, 0x02, 0xaa, 0xbb])
            .expect("generic message must parse");

        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].number, 1);
        assert!(matches!(fields[0].value, FieldValue::Varint(150)));
        assert_eq!(fields[1].number, 2);
        assert!(matches!(fields[1].value, FieldValue::Bytes(&[0xaa, 0xbb])));
    }

    #[test]
    fn rejects_overlong_varint() {
        assert!(decode_varint(&[0x80; 10]).is_err());
    }
}
