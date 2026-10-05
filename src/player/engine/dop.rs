// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! DoP (DSD over PCM) encoder.
//!
//! Packs DSD data into 24-bit PCM frames for transmission over standard PCM
//! audio interfaces. The DAC recognizes the DoP markers and extracts the
//! original DSD stream.
//!
//! DoP Format:
//! - DSD64 (2.8224 MHz) -> 176.4 kHz PCM (2.8224 / 16 = 176.4)
//! - DSD128 (5.6448 MHz) -> 352.8 kHz PCM (5.6448 / 16 = 352.8)
//!
//! Each PCM sample contains:
//! - Byte 0: Marker (0x05 or 0xFA alternating)
//! - Byte 1: 8 bits of DSD data
//! - Byte 2: 8 more bits of DSD data
//!
//! Reference: <https://dsd-guide.com/sites/default/files/white-papers/DoP_openStandard_1v1.pdf>
//!
//! This module has no production dependencies beyond the Symphonia fork's
//! `BitOrder`/`ChannelDataLayout` types and lyra's [`PlayerError`] for its
//! error return. There's deliberately no DoP-to-`f32` conversion helper —
//! DoP output only ever reaches cpal as 24-bit-in-32-bit samples, so that
//! conversion has no production caller.

use symphonia::core::codecs::audio::{BitOrder, ChannelDataLayout};

use crate::player::backend::PlayerError;

pub type Result<T, E = PlayerError> = std::result::Result<T, E>;

const DOP_MARKER_1: u8 = 0x05;
const DOP_MARKER_2: u8 = 0xFA;

/// Lookup table for bit reversal (LSB-first to MSB-first).
/// Generated at compile time. Each index maps to its bit-reversed value.
const BIT_REVERSE_TABLE: [u8; 256] = {
    let mut table = [0u8; 256];
    let mut i = 0u16;
    while i < 256 {
        let mut result = 0u8;
        let mut b = i as u8;
        let mut bit = 0;
        while bit < 8 {
            result = (result << 1) | (b & 1);
            b >>= 1;
            bit += 1;
        }
        table[i as usize] = result;
        i += 1;
    }
    table
};

/// Reverse the bits in a byte using lookup table.
/// Used when source DSD is LSB-first but DAC expects MSB-first in DoP.
#[inline]
fn reverse_bits(byte: u8) -> u8 {
    BIT_REVERSE_TABLE[byte as usize]
}

/// DoP encoder that converts DSD data to DoP-encoded PCM.
pub struct DopEncoder {
    dsd_sample_rate: u32,
    channels: usize,
    marker_toggle: bool,
    channel_layout: ChannelDataLayout,
    bit_order: BitOrder,
}

impl DopEncoder {
    /// Only DSD64 (2,822,400 Hz) and DSD128 (5,644,800 Hz) are supported.
    /// DSD256 would require 705.6kHz PCM output, which no consumer DAC or
    /// audio interface supports over DoP.
    pub fn new(
        dsd_sample_rate: u32,
        channels: usize,
        channel_layout: ChannelDataLayout,
        bit_order: BitOrder,
    ) -> Result<Self> {
        // Validate DSD sample rate
        match dsd_sample_rate {
            2822400 => {} // DSD64
            5644800 => {} // DSD128
            11289600 => {
                return Err(PlayerError(
                    "DSD256 not supported via DoP (would require 705.6kHz PCM)".to_owned(),
                ));
            }
            _ => {
                return Err(PlayerError(format!(
                    "Unsupported DSD sample rate: {dsd_sample_rate}"
                )));
            }
        }

        Ok(Self {
            dsd_sample_rate,
            channels,
            marker_toggle: false,
            channel_layout,
            bit_order,
        })
    }

    /// Get the PCM sample rate for DoP encoding.
    pub fn pcm_sample_rate(&self) -> u32 {
        // Each PCM frame contains 16 DSD bits (2 bytes)
        self.dsd_sample_rate / 16
    }

    /// Convert DSD samples to DoP-encoded 24-bit PCM samples.
    ///
    /// Input: Raw DSD data (1 bit per sample, packed into bytes)
    /// Output: 24-bit PCM samples (as i32) ready for playback
    ///
    /// Handles both planar and interleaved channel layouts.
    /// Handles both LSB-first and MSB-first bit orders.
    ///
    /// DoP PCM output (always interleaved):
    /// `[L_marker_DSD0-15, R_marker_DSD0-15, ...]` for each frame
    pub fn encode(&mut self, dsd_data: &[u8], output: &mut Vec<i32>) {
        // Each DoP PCM sample contains 16 DSD bits (2 bytes per channel)
        let dsd_bytes_per_channel_per_frame = 2;

        match self.channel_layout {
            ChannelDataLayout::Planar => {
                // Planar layout: all bytes for channel 0, then all for channel 1
                let bytes_per_channel = dsd_data.len() / self.channels;
                let num_frames = bytes_per_channel / dsd_bytes_per_channel_per_frame;

                // Reserve space for output (one sample per channel per frame, interleaved)
                output.clear();
                output.reserve(num_frames * self.channels);

                for frame_idx in 0..num_frames {
                    // Alternate marker for each frame
                    let marker = if self.marker_toggle {
                        DOP_MARKER_2
                    } else {
                        DOP_MARKER_1
                    };
                    self.marker_toggle = !self.marker_toggle;

                    // Process each channel (convert planar to interleaved output)
                    for ch in 0..self.channels {
                        // Calculate offset in planar layout
                        let channel_offset = ch * bytes_per_channel;
                        let dsd_offset =
                            channel_offset + (frame_idx * dsd_bytes_per_channel_per_frame);

                        // Get 2 bytes of DSD data for this channel
                        let dsd_byte1 = dsd_data[dsd_offset];
                        let dsd_byte2 = dsd_data[dsd_offset + 1];

                        // Apply bit reversal (LSB-first -> MSB-first)
                        let (byte1, byte2) = if self.bit_order == BitOrder::LsbFirst {
                            (reverse_bits(dsd_byte1), reverse_bits(dsd_byte2))
                        } else {
                            (dsd_byte1, dsd_byte2)
                        };

                        // Pack into 24-bit DoP sample: [marker, byte1, byte2]
                        // For 32-bit output (S32_LE), left-align by shifting left 8 bits
                        // Result: [marker][byte1][byte2][0x00]
                        let dop_sample = ((marker as i32) << 24)
                            | ((byte1 as i32) << 16)
                            | ((byte2 as i32) << 8);

                        output.push(dop_sample);
                    }
                }
            }
            ChannelDataLayout::Interleaved => {
                // Interleaved layout: [L0, R0, L1, R1, L2, R2, ...]
                let num_frames = dsd_data.len() / (self.channels * dsd_bytes_per_channel_per_frame);

                // Reserve space for output
                output.clear();
                output.reserve(num_frames * self.channels);

                for frame_idx in 0..num_frames {
                    // Alternate marker for each frame
                    let marker = if self.marker_toggle {
                        DOP_MARKER_2
                    } else {
                        DOP_MARKER_1
                    };
                    self.marker_toggle = !self.marker_toggle;

                    // Process each channel
                    for ch in 0..self.channels {
                        // Calculate offset in interleaved layout
                        let dsd_offset =
                            (frame_idx * self.channels + ch) * dsd_bytes_per_channel_per_frame;

                        // Get 2 bytes of DSD data for this channel
                        let dsd_byte1 = dsd_data[dsd_offset];
                        let dsd_byte2 = dsd_data[dsd_offset + 1];

                        // Apply bit reversal (LSB-first -> MSB-first)
                        let (byte1, byte2) = if self.bit_order == BitOrder::LsbFirst {
                            (reverse_bits(dsd_byte1), reverse_bits(dsd_byte2))
                        } else {
                            (dsd_byte1, dsd_byte2)
                        };

                        // Pack into 24-bit DoP sample: [marker, byte1, byte2]
                        // For 32-bit output (S32_LE), left-align by shifting left 8 bits
                        // Result: [marker][byte1][byte2][0x00]
                        let dop_sample = ((marker as i32) << 24)
                            | ((byte1 as i32) << 16)
                            | ((byte2 as i32) << 8);

                        output.push(dop_sample);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dop_encoder_dsd64() {
        // Test with MSB-first bit order (no reversal needed)
        let mut encoder =
            DopEncoder::new(2822400, 2, ChannelDataLayout::Planar, BitOrder::MsbFirst).unwrap();
        assert_eq!(encoder.pcm_sample_rate(), 176400);

        // Test data: 4 bytes planar format (all left, then all right)
        // Left channel: [0x12, 0x34]
        // Right channel: [0x56, 0x78]
        let dsd_data = vec![0x12, 0x34, 0x56, 0x78];
        let mut output = Vec::new();

        encoder.encode(&dsd_data, &mut output);

        // Should produce 2 samples: L and R for one frame
        assert_eq!(output.len(), 2);

        // First frame, left channel: [marker1][0x12][0x34][0x00] (left-aligned)
        assert_eq!(
            output[0],
            (DOP_MARKER_1 as i32) << 24 | 0x12 << 16 | 0x34 << 8
        );

        // First frame, right channel: [marker1][0x56][0x78][0x00] (left-aligned)
        assert_eq!(
            output[1],
            (DOP_MARKER_1 as i32) << 24 | 0x56 << 16 | 0x78 << 8
        );
    }

    #[test]
    fn test_marker_alternation() {
        // Test with MSB-first bit order
        let mut encoder =
            DopEncoder::new(2822400, 1, ChannelDataLayout::Planar, BitOrder::MsbFirst).unwrap();
        let dsd_data = vec![0xAA, 0xBB, 0xCC, 0xDD];
        let mut output = Vec::new();

        encoder.encode(&dsd_data, &mut output);

        // Extract markers from samples (now in MSB due to left-alignment)
        let marker1 = (output[0] >> 24) as u8;
        let marker2 = (output[1] >> 24) as u8;

        // Markers should alternate
        assert_eq!(marker1, DOP_MARKER_1);
        assert_eq!(marker2, DOP_MARKER_2);
    }

    #[test]
    fn test_interleaved_layout_and_lsb_first_reversal() {
        // LSB-first source must be bit-reversed to MSB-first in the DoP output.
        let mut encoder = DopEncoder::new(
            2822400,
            2,
            ChannelDataLayout::Interleaved,
            BitOrder::LsbFirst,
        )
        .unwrap();
        // Interleaved: L0,L1 = 0x01,0x02 ; R0,R1 = 0x03,0x04 for frame 0
        let dsd_data = vec![0x01, 0x02, 0x03, 0x04];
        let mut output = Vec::new();

        encoder.encode(&dsd_data, &mut output);

        assert_eq!(output.len(), 2);
        let expected_l = ((DOP_MARKER_1 as i32) << 24)
            | ((reverse_bits(0x01) as i32) << 16)
            | ((reverse_bits(0x02) as i32) << 8);
        let expected_r = ((DOP_MARKER_1 as i32) << 24)
            | ((reverse_bits(0x03) as i32) << 16)
            | ((reverse_bits(0x04) as i32) << 8);
        assert_eq!(output[0], expected_l);
        assert_eq!(output[1], expected_r);
    }

    #[test]
    fn test_rejects_dsd256() {
        let result = DopEncoder::new(11289600, 2, ChannelDataLayout::Planar, BitOrder::MsbFirst);
        assert!(result.is_err());
    }

    #[test]
    fn test_rejects_unsupported_rate() {
        let result = DopEncoder::new(48000, 2, ChannelDataLayout::Planar, BitOrder::MsbFirst);
        assert!(result.is_err());
    }

    #[test]
    fn test_bit_reverse_table_is_involution() {
        // Reversing twice must return the original byte for every possible value.
        for b in 0u8..=255 {
            assert_eq!(reverse_bits(reverse_bits(b)), b);
        }
        // Spot-check a couple of well-known reversals.
        assert_eq!(reverse_bits(0b0000_0001), 0b1000_0000);
        assert_eq!(reverse_bits(0b1111_0000), 0b0000_1111);
    }
}
