// SPDX-License-Identifier: GPL-3.0-only

//! Clean-room wire messages and authenticated-plaintext fragmentation.

mod fragment;
mod message;

pub use fragment::{
    Fragment, FragmentAccumulator, FragmentError, FragmentHeader, MAX_FRAGMENT_BODY_BYTES,
};
pub use message::{
    ByteRun, Instruction, InstructionBatch, Marker, MessageError, StateUpdate, ViewportSize,
    decode_compressed_update, encode_compressed_update,
};
