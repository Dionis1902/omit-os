// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! The system's random number generator, without the hardware.
//!
//! The kernel feeds a [`Generator`] whatever unpredictable values the
//! machine offers (the processor's random number instruction, the
//! timing of interrupts) and takes random bytes out of it for programs.
//!
//! The generator is the ChaCha20 block function of RFC 8439 used in the
//! well-known "fast key erasure" way: output is ChaCha20 key stream
//! under a secret key, and before any output is handed over the key is
//! replaced by the first 32 bytes of that key stream — so that what was
//! given out before cannot be worked out from the generator's state
//! later. Input is mixed in by adding it to the key and replacing the
//! key in the same way.
//!
//! **This construction has not been reviewed by a cryptographer.** The
//! block function is checked against the RFC's test vector; how well
//! the generator is seeded is the caller's business and is not judged
//! here.

#![no_std]

#[cfg(test)]
extern crate std;

/// The four constant words every ChaCha20 state begins with:
/// "expand 32-byte k".
const CONSTANTS: [u32; 4] = [0x6170_7865, 0x3320_646E, 0x7962_2D32, 0x6B20_6574];

/// Bytes in one block of key stream.
pub const BLOCK: usize = 64;

fn quarter_round(state: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    state[a] = state[a].wrapping_add(state[b]);
    state[d] = (state[d] ^ state[a]).rotate_left(16);
    state[c] = state[c].wrapping_add(state[d]);
    state[b] = (state[b] ^ state[c]).rotate_left(12);
    state[a] = state[a].wrapping_add(state[b]);
    state[d] = (state[d] ^ state[a]).rotate_left(8);
    state[c] = state[c].wrapping_add(state[d]);
    state[b] = (state[b] ^ state[c]).rotate_left(7);
}

/// The ChaCha20 block function (RFC 8439, section 2.3): 64 bytes of key
/// stream for a key, a block counter and a nonce.
pub fn chacha20_block(key: &[u32; 8], counter: u32, nonce: &[u32; 3]) -> [u8; BLOCK] {
    let mut initial = [0u32; 16];
    initial[..4].copy_from_slice(&CONSTANTS);
    initial[4..12].copy_from_slice(key);
    initial[12] = counter;
    initial[13..].copy_from_slice(nonce);
    let mut state = initial;
    for _ in 0..10 {
        // A column round, then a diagonal round.
        quarter_round(&mut state, 0, 4, 8, 12);
        quarter_round(&mut state, 1, 5, 9, 13);
        quarter_round(&mut state, 2, 6, 10, 14);
        quarter_round(&mut state, 3, 7, 11, 15);
        quarter_round(&mut state, 0, 5, 10, 15);
        quarter_round(&mut state, 1, 6, 11, 12);
        quarter_round(&mut state, 2, 7, 8, 13);
        quarter_round(&mut state, 3, 4, 9, 14);
    }
    let mut out = [0u8; BLOCK];
    for (index, (word, start)) in state.iter().zip(initial).enumerate() {
        out[index * 4..index * 4 + 4].copy_from_slice(&word.wrapping_add(start).to_le_bytes());
    }
    out
}

/// The nonce of blocks that replace the key after input was mixed in,
/// and of blocks that produce output: the two never share key stream.
const MIXING: [u32; 3] = [0x6D69_7869, 0x6E67_2020, 0];
const OUTPUT: [u32; 3] = [0x6F75_7470, 0x7574_2020, 0];

/// A random number generator: see the crate's description.
#[derive(Clone)]
pub struct Generator {
    key: [u32; 8],
    /// Bytes mixed in so far (it only ever says how much, not how good).
    mixed: u64,
}

impl Default for Generator {
    fn default() -> Self {
        Self::new()
    }
}

impl Generator {
    /// A generator nothing has been mixed into: its output is the same
    /// every time until something is.
    pub const fn new() -> Self {
        Self {
            key: [0; 8],
            mixed: 0,
        }
    }

    /// How many bytes have been mixed in.
    pub fn mixed(&self) -> u64 {
        self.mixed
    }

    fn rekey(&mut self, nonce: &[u32; 3]) {
        let block = chacha20_block(&self.key, 0, nonce);
        for (word, bytes) in self.key.iter_mut().zip(block.chunks_exact(4)) {
            *word = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        }
    }

    /// Mixes `input` into the generator's state. However little is known
    /// about `input`, what comes out afterwards is no easier to predict
    /// than before.
    pub fn mix(&mut self, input: &[u8]) {
        for chunk in input.chunks(32) {
            for (index, &byte) in chunk.iter().enumerate() {
                self.key[index / 4] ^= u32::from(byte) << (8 * (index % 4));
            }
            self.rekey(&MIXING);
        }
        self.mixed = self.mixed.saturating_add(input.len() as u64);
    }

    /// Fills `out` with random bytes.
    pub fn fill(&mut self, out: &mut [u8]) {
        // The key that produces this output is gone before the output
        // is: block 0 becomes the next key, the blocks after it are
        // handed out.
        let key = self.key;
        self.rekey(&OUTPUT);
        for (index, chunk) in out.chunks_mut(BLOCK).enumerate() {
            // (2^32 blocks are 256 GiB: more than one call can ask for.)
            let block = chacha20_block(&key, index as u32 + 1, &OUTPUT);
            chunk.copy_from_slice(&block[..chunk.len()]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;
    use std::vec::Vec;

    /// RFC 8439, section 2.3.2.
    #[test]
    fn the_block_function_matches_the_rfc() {
        let mut key = [0u32; 8];
        for (index, word) in key.iter_mut().enumerate() {
            let first = index as u8 * 4;
            *word = u32::from_le_bytes([first, first + 1, first + 2, first + 3]);
        }
        let nonce = [0x0900_0000, 0x4A00_0000, 0];
        let block = chacha20_block(&key, 1, &nonce);
        let expected: [u8; 64] = [
            0x10, 0xf1, 0xe7, 0xe4, 0xd1, 0x3b, 0x59, 0x15, 0x50, 0x0f, 0xdd, 0x1f, 0xa3, 0x20,
            0x71, 0xc4, 0xc7, 0xd1, 0xf4, 0xc7, 0x33, 0xc0, 0x68, 0x03, 0x04, 0x22, 0xaa, 0x9a,
            0xc3, 0xd4, 0x6c, 0x4e, 0xd2, 0x82, 0x64, 0x46, 0x07, 0x9f, 0xaa, 0x09, 0x14, 0xc2,
            0xd7, 0x05, 0xd9, 0x8b, 0x02, 0xa2, 0xb5, 0x12, 0x9c, 0xd1, 0xde, 0x16, 0x4e, 0xb9,
            0xcb, 0xd0, 0x83, 0xe8, 0xa2, 0x50, 0x3c, 0x4e,
        ];
        assert_eq!(block, expected);
    }

    fn take(generator: &mut Generator, length: usize) -> Vec<u8> {
        let mut out = vec![0u8; length];
        generator.fill(&mut out);
        out
    }

    #[test]
    fn output_never_repeats_and_depends_on_everything_mixed_in() {
        let mut a = Generator::new();
        a.mix(b"some seed");
        let mut b = a.clone();
        // The same state gives the same bytes ...
        let first = take(&mut a, 100);
        assert_eq!(first, take(&mut b, 100));
        // ... and then different ones, every time.
        let second = take(&mut a, 100);
        assert_ne!(first, second);
        assert_ne!(second, take(&mut a, 100));
        // One bit of difference in what was mixed in changes it all,
        // also beyond the first 32 bytes of input and whatever came
        // before.
        let seeded = |input: &[u8]| {
            let mut generator = Generator::new();
            generator.mix(b"before");
            generator.mix(input);
            take(&mut generator, 64)
        };
        let mut long = [7u8; 80];
        let one = seeded(&long);
        long[79] ^= 1;
        let other = seeded(&long);
        assert_ne!(one, other);
        let same_bits = one
            .iter()
            .zip(&other)
            .map(|(x, y)| (x ^ y).count_zeros())
            .sum::<u32>();
        // About half of the 512 bits differ.
        assert!((180..=330).contains(&same_bits), "{same_bits}");
        assert_eq!(Generator::new().mixed(), 0);
        let mut counted = Generator::new();
        counted.mix(&long);
        assert_eq!(counted.mixed(), 80);
    }

    #[test]
    fn long_requests_are_one_stream_and_short_ones_are_not_its_prefix() {
        let mut generator = Generator::new();
        generator.mix(b"seed");
        let mut again = generator.clone();
        let long = take(&mut generator, 300);
        // No block of the output repeats another.
        let blocks: Vec<&[u8]> = long.chunks(64).collect();
        for (index, block) in blocks.iter().enumerate() {
            assert!(!blocks[..index].contains(block));
        }
        assert!(long.iter().any(|&byte| byte != 0));
        // Asking for less gives the beginning of the same stream (the
        // state before was the same), but the next request starts
        // afresh rather than continuing it.
        assert_eq!(take(&mut again, 10), long[..10]);
        assert_ne!(take(&mut again, 10), long[10..20]);
    }

    /// What was handed out cannot be recomputed from the state the
    /// generator is left in: the key that made it is not there any more.
    #[test]
    fn the_key_is_replaced_before_output_is_handed_over() {
        let mut generator = Generator::new();
        generator.mix(b"seed");
        let before = generator.key;
        let out = take(&mut generator, 64);
        assert_ne!(generator.key, before);
        // The output was made with the old key ...
        assert_eq!(out, chacha20_block(&before, 1, &OUTPUT));
        // ... which the new one does not reproduce.
        assert_ne!(out, chacha20_block(&generator.key, 1, &OUTPUT));
    }
}
