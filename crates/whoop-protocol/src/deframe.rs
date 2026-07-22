//! Length-based reassembly for one notify characteristic. BLE delivers frames in MTU-sized
//! fragments and packs short frames together, so the boundary a notification arrives on is not
//! the boundary a frame ends on.

use alloc::vec::Vec;

use crate::{
    decode_frame, Generation, ProtocolError, MAX_FRAME_BYTES, START_OF_FRAME, TRAILER_BYTES,
};

/// Buffers fragments for a single characteristic and yields whole frames as they complete.
///
/// Hold one per notify characteristic: interleaved channels each carry their own frame boundaries
/// and a shared buffer would splice them together. Resynchronisation is by start byte and declared
/// length, never by a delimiter — payload bytes are freely `0xaa`.
#[derive(Debug)]
pub struct Deframer {
    generation: Generation,
    buf: Vec<u8>,
    head: usize,
}

impl Deframer {
    pub fn new(generation: Generation) -> Self {
        Self {
            generation,
            buf: Vec::new(),
            head: 0,
        }
    }

    /// Drop any buffered partial frame. Call on connect, resume, and state restore so a frame
    /// truncated by a dropped link cannot wedge the next session.
    pub fn reset(&mut self) {
        self.buf.clear();
        self.head = 0;
    }

    /// Append a notification and return every frame that completed, in arrival order. A frame
    /// whose CRC fails is returned as an error rather than dropped, and the stream behind it stays
    /// aligned because the declared length was still consumed.
    pub fn push(&mut self, data: &[u8]) -> Vec<Result<Vec<u8>, ProtocolError>> {
        self.buf.extend_from_slice(data);
        let header_len = self.generation.header_len();
        let length_offset = self.generation.length_offset();
        let mut out = Vec::new();

        loop {
            while self.head < self.buf.len() && self.buf[self.head] != START_OF_FRAME {
                self.head += 1;
            }
            let Some(available) = self.buf.len().checked_sub(self.head) else {
                break;
            };
            if available < length_offset + 2 {
                break;
            }
            let Some(bytes) = self
                .buf
                .get(self.head + length_offset..self.head + length_offset + 2)
            else {
                break;
            };
            let declared = usize::from(u16::from_le_bytes([bytes[0], bytes[1]]));
            let total = header_len.saturating_add(declared);
            if declared < TRAILER_BYTES || total > MAX_FRAME_BYTES {
                self.head += 1;
                continue;
            }
            if available < total {
                break;
            }
            let end = self.head + total;
            let Some(frame) = self.buf.get(self.head..end) else {
                break;
            };
            out.push(decode_frame(self.generation, frame));
            self.head = end;
        }

        if self.head > 0 {
            self.buf.drain(..self.head);
            self.head = 0;
        }
        out
    }
}
