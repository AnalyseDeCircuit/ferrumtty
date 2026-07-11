// SPDX-License-Identifier: GPL-3.0-only

use std::collections::BTreeMap;

const FRAGMENT_HEADER_BYTES: usize = 14;
const FINAL_FRAGMENT_BIT: u16 = 1_u16 << 15;
const FRAGMENT_INDEX_MASK: u16 = FINAL_FRAGMENT_BIT - 1;
pub const MAX_FRAGMENT_BODY_BYTES: usize = 1214;

/// Metadata outside the compressed state message but inside authentication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FragmentHeader {
    pub sent_timestamp: u16,
    pub echoed_timestamp: u16,
    pub state_id: u64,
    pub index: u16,
    pub is_final: bool,
}

/// An authenticated plaintext fragment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Fragment {
    pub header: FragmentHeader,
    pub body: Vec<u8>,
}

impl Fragment {
    /// Parses the fixed 14-byte prefix after packet authentication.
    ///
    /// # Errors
    ///
    /// Returns an error when the plaintext is shorter than the prefix or a
    /// non-final fragment does not use the observed full body size.
    pub fn parse(plaintext: &[u8]) -> Result<Self, FragmentError> {
        let header_bytes = plaintext
            .get(..FRAGMENT_HEADER_BYTES)
            .ok_or(FragmentError::HeaderTooShort)?;
        let fragment_word = u16::from_be_bytes([header_bytes[12], header_bytes[13]]);
        let header = FragmentHeader {
            sent_timestamp: u16::from_be_bytes([header_bytes[0], header_bytes[1]]),
            echoed_timestamp: u16::from_be_bytes([header_bytes[2], header_bytes[3]]),
            state_id: u64::from_be_bytes(
                header_bytes[4..12]
                    .try_into()
                    .map_err(|_| FragmentError::HeaderTooShort)?,
            ),
            index: fragment_word & FRAGMENT_INDEX_MASK,
            is_final: fragment_word & FINAL_FRAGMENT_BIT != 0,
        };
        let body = plaintext[FRAGMENT_HEADER_BYTES..].to_vec();
        if body.len() > MAX_FRAGMENT_BODY_BYTES {
            return Err(FragmentError::BodyTooLarge);
        }
        if !header.is_final && body.len() != MAX_FRAGMENT_BODY_BYTES {
            return Err(FragmentError::ShortNonFinalBody);
        }
        Ok(Self { header, body })
    }

    /// Splits a compressed state message using the observed 1214-byte boundary.
    ///
    /// # Errors
    ///
    /// Returns an error if the message requires more than 32,768 fragments.
    pub fn split(
        message: &[u8],
        sent_timestamp: u16,
        echoed_timestamp: u16,
        state_id: u64,
    ) -> Result<Vec<Self>, FragmentError> {
        let chunk_count = message.len().max(1).div_ceil(MAX_FRAGMENT_BODY_BYTES);
        if chunk_count > usize::from(FRAGMENT_INDEX_MASK) + 1 {
            return Err(FragmentError::TooManyFragments);
        }

        if message.is_empty() {
            return Ok(vec![Self {
                header: FragmentHeader {
                    sent_timestamp,
                    echoed_timestamp,
                    state_id,
                    index: 0,
                    is_final: true,
                },
                body: Vec::new(),
            }]);
        }

        message
            .chunks(MAX_FRAGMENT_BODY_BYTES)
            .enumerate()
            .map(|(index, body)| {
                Ok(Self {
                    header: FragmentHeader {
                        sent_timestamp,
                        echoed_timestamp,
                        state_id,
                        index: u16::try_from(index).map_err(|_| FragmentError::TooManyFragments)?,
                        is_final: index + 1 == chunk_count,
                    },
                    body: body.to_vec(),
                })
            })
            .collect()
    }

    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut output = Vec::with_capacity(FRAGMENT_HEADER_BYTES + self.body.len());
        output.extend_from_slice(&self.header.sent_timestamp.to_be_bytes());
        output.extend_from_slice(&self.header.echoed_timestamp.to_be_bytes());
        output.extend_from_slice(&self.header.state_id.to_be_bytes());
        let final_bit = if self.header.is_final {
            FINAL_FRAGMENT_BIT
        } else {
            0
        };
        output.extend_from_slice(&(self.header.index | final_bit).to_be_bytes());
        output.extend_from_slice(&self.body);
        output
    }
}

/// Bounded accumulator for one state identifier, tolerant of packet reordering.
pub struct FragmentAccumulator {
    state_id: u64,
    maximum_message_bytes: usize,
    fragments: BTreeMap<u16, Vec<u8>>,
    final_index: Option<u16>,
    total_bytes: usize,
}

impl FragmentAccumulator {
    #[must_use]
    pub fn new(state_id: u64, maximum_message_bytes: usize) -> Self {
        Self {
            state_id,
            maximum_message_bytes,
            fragments: BTreeMap::new(),
            final_index: None,
            total_bytes: 0,
        }
    }

    /// Adds one authenticated fragment and returns a complete message once ready.
    ///
    /// # Errors
    ///
    /// Returns an error for another state identifier, conflicting duplicates,
    /// inconsistent final indexes, fragments beyond the final index, or a
    /// message larger than the configured bound.
    pub fn push(&mut self, fragment: Fragment) -> Result<Option<Vec<u8>>, FragmentError> {
        if fragment.header.state_id != self.state_id {
            return Err(FragmentError::MismatchedState);
        }
        if self
            .final_index
            .is_some_and(|final_index| fragment.header.index > final_index)
        {
            return Err(FragmentError::IndexAfterFinal);
        }
        if fragment.header.is_final {
            if self
                .final_index
                .is_some_and(|final_index| final_index != fragment.header.index)
            {
                return Err(FragmentError::ConflictingFinalIndex);
            }
            if self
                .fragments
                .keys()
                .any(|index| *index > fragment.header.index)
            {
                return Err(FragmentError::IndexAfterFinal);
            }
            self.final_index = Some(fragment.header.index);
        }

        if let Some(existing) = self.fragments.get(&fragment.header.index) {
            return if existing == &fragment.body {
                self.try_complete()
            } else {
                Err(FragmentError::ConflictingDuplicate)
            };
        }

        self.total_bytes = self
            .total_bytes
            .checked_add(fragment.body.len())
            .ok_or(FragmentError::MessageTooLarge)?;
        if self.total_bytes > self.maximum_message_bytes {
            return Err(FragmentError::MessageTooLarge);
        }
        self.fragments.insert(fragment.header.index, fragment.body);
        self.try_complete()
    }

    fn try_complete(&self) -> Result<Option<Vec<u8>>, FragmentError> {
        let Some(final_index) = self.final_index else {
            return Ok(None);
        };
        if self.fragments.len() != usize::from(final_index) + 1 {
            return Ok(None);
        }

        let mut message = Vec::with_capacity(self.total_bytes);
        for index in 0..=final_index {
            let body = self
                .fragments
                .get(&index)
                .ok_or(FragmentError::MissingFragment)?;
            message.extend_from_slice(body);
        }
        Ok(Some(message))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FragmentError {
    HeaderTooShort,
    BodyTooLarge,
    ShortNonFinalBody,
    TooManyFragments,
    MismatchedState,
    IndexAfterFinal,
    ConflictingFinalIndex,
    ConflictingDuplicate,
    MessageTooLarge,
    MissingFragment,
}

#[cfg(test)]
mod tests {
    use super::{Fragment, FragmentAccumulator, FragmentError, MAX_FRAGMENT_BODY_BYTES};

    #[test]
    fn splits_and_reassembles_out_of_order() {
        let message = vec![0x5a; MAX_FRAGMENT_BODY_BYTES * 2 + 17];
        let fragments = Fragment::split(&message, 0x1234, 0xffff, 7).expect("split must work");
        assert_eq!(fragments.len(), 3);
        assert!(!fragments[0].header.is_final);
        assert!(fragments[2].header.is_final);

        let mut accumulator = FragmentAccumulator::new(7, message.len());
        assert_eq!(accumulator.push(fragments[1].clone()), Ok(None));
        assert_eq!(accumulator.push(fragments[2].clone()), Ok(None));
        assert_eq!(accumulator.push(fragments[0].clone()), Ok(Some(message)));
    }

    #[test]
    fn observed_prefix_parses_and_reencodes() {
        let plaintext = decode_hex("e5a2ffff00000000000000020000");
        let mut plaintext = plaintext;
        plaintext.extend(vec![0x11; MAX_FRAGMENT_BODY_BYTES]);

        let fragment = Fragment::parse(&plaintext).expect("observed prefix must parse");

        assert_eq!(fragment.header.sent_timestamp, 0xe5a2);
        assert_eq!(fragment.header.echoed_timestamp, 0xffff);
        assert_eq!(fragment.header.state_id, 2);
        assert_eq!(fragment.header.index, 0);
        assert!(!fragment.header.is_final);
        assert_eq!(fragment.encode(), plaintext);
    }

    #[test]
    fn rejects_conflicting_duplicate() {
        let mut fragments = Fragment::split(b"state", 1, 2, 3).expect("split must work");
        let original = fragments.remove(0);
        let mut conflicting = original.clone();
        conflicting.body[0] ^= 1;
        let mut accumulator = FragmentAccumulator::new(3, 100);
        assert!(accumulator.push(original).is_ok());

        assert_eq!(
            accumulator.push(conflicting),
            Err(FragmentError::ConflictingDuplicate)
        );
    }

    fn decode_hex(value: &str) -> Vec<u8> {
        value
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| (nibble(pair[0]) << 4) | nibble(pair[1]))
            .collect()
    }

    fn nibble(value: u8) -> u8 {
        match value {
            b'0'..=b'9' => value - b'0',
            b'a'..=b'f' => value - b'a' + 10,
            _ => panic!("test vector contains invalid hex"),
        }
    }
}
