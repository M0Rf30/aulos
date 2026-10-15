// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Channel-count conversion (downmix / upmix) applied to the decoded PCM
//! before resampling and encoding.
//!
//! Only mono and stereo are offered as targets, so the only upmix is
//! mono → stereo (the sample is duplicated); everything else is a downmix.
//! Multichannel sources are folded down assuming the usual SMPTE/ITU channel
//! order (L R C LFE surrounds…) — the pipeline only knows a channel *count*,
//! not a layout. The LFE channel is dropped, as in a standard stereo
//! downmix, and every output row is normalized to unit gain so the mix can
//! never clip when the inputs don't.

use serde::{Deserialize, Serialize};

/// Output channel layout of a conversion job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ChannelMode {
    /// Keep the source's channel count.
    #[default]
    Source,
    Mono,
    Stereo,
}

impl ChannelMode {
    pub const ALL: [ChannelMode; 3] = [Self::Source, Self::Mono, Self::Stereo];

    /// Channel count of the output for a source with `source` channels.
    #[must_use]
    pub fn output_channels(self, source: u16) -> u16 {
        match self {
            Self::Source => source,
            Self::Mono => 1,
            Self::Stereo => 2,
        }
    }
}

/// A fixed `src → dst` channel mix: `matrix` holds `dst` rows of `src` gains.
#[derive(Debug, Clone, PartialEq)]
pub struct Mixer {
    src: usize,
    dst: usize,
    matrix: Vec<f32>,
}

impl Mixer {
    /// The mixer turning `src` channels into `dst`, or `None` when no
    /// processing is needed (same count, or a zero count on either side).
    #[must_use]
    pub fn new(src: u16, dst: u16) -> Option<Self> {
        if src == dst || src == 0 || dst == 0 {
            return None;
        }
        let (s, d) = (usize::from(src), usize::from(dst));
        let matrix = match d {
            1 => mono_row(s),
            2 => stereo_rows(s),
            // Only mono/stereo targets exist; for anything else keep the
            // leading channels (zero-filled when upmixing).
            _ => (0..d)
                .flat_map(|o| (0..s).map(move |i| f32::from(u8::from(i == o))))
                .collect(),
        };
        Some(Self {
            src: s,
            dst: d,
            matrix,
        })
    }

    /// Mixes interleaved `input` (a whole number of `src`-channel frames)
    /// into `out`, replacing its contents.
    pub fn process(&self, input: &[f32], out: &mut Vec<f32>) {
        out.clear();
        out.reserve(input.len() / self.src * self.dst);
        for frame in input.chunks_exact(self.src) {
            for row in self.matrix.chunks_exact(self.src) {
                out.push(row.iter().zip(frame).map(|(g, s)| g * s).sum());
            }
        }
    }
}

/// Gains of the single mono row for `src` channels: the average of the
/// stereo downmix's two rows.
fn mono_row(src: usize) -> Vec<f32> {
    if src == 1 {
        return vec![1.0];
    }
    let rows = stereo_rows(src);
    let (left, right) = rows.split_at(src);
    left.iter().zip(right).map(|(l, r)| 0.5 * (l + r)).collect()
}

/// The two stereo rows (left then right) for `src` channels.
fn stereo_rows(src: usize) -> Vec<f32> {
    if src == 1 {
        return vec![1.0, 1.0];
    }
    const C: f32 = std::f32::consts::FRAC_1_SQRT_2;
    let mut left = vec![0.0f32; src];
    let mut right = vec![0.0f32; src];
    match src {
        // L R C … : centre into both sides, surrounds on their own side.
        5 | 6 => {
            left[0] = 1.0;
            right[1] = 1.0;
            left[2] = C;
            right[2] = C;
            let surround = if src == 5 { 3 } else { 4 };
            left[surround] = C;
            right[surround + 1] = C;
        }
        // 6.1 (L R C LFE Ls Rs Cs) and 7.1 (… Ls Rs Lb Rb): side/back pairs
        // share the surround weight.
        7 | 8 => {
            left[0] = 1.0;
            right[1] = 1.0;
            left[2] = C;
            right[2] = C;
            if src == 8 {
                left[4] = C / 2.0;
                right[5] = C / 2.0;
                left[6] = C / 2.0;
                right[7] = C / 2.0;
            } else {
                left[4] = C;
                right[5] = C;
                left[6] = C / 2.0;
                right[6] = C / 2.0;
            }
        }
        // Unknown layout: even channels left, odd channels right.
        _ => {
            for i in 0..src {
                if i % 2 == 0 {
                    left[i] = 1.0;
                } else {
                    right[i] = 1.0;
                }
            }
        }
    }
    for row in [&mut left, &mut right] {
        let sum: f32 = row.iter().sum();
        if sum > 0.0 {
            row.iter_mut().for_each(|g| *g /= sum);
        }
    }
    left.extend(right);
    left
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mix(src: u16, dst: u16, input: &[f32]) -> Vec<f32> {
        let mut out = Vec::new();
        Mixer::new(src, dst).unwrap().process(input, &mut out);
        out
    }

    #[test]
    fn output_channels_follows_the_mode() {
        assert_eq!(ChannelMode::Source.output_channels(6), 6);
        assert_eq!(ChannelMode::Mono.output_channels(6), 1);
        assert_eq!(ChannelMode::Stereo.output_channels(1), 2);
    }

    #[test]
    fn same_or_empty_channel_counts_need_no_mixer() {
        assert!(Mixer::new(2, 2).is_none());
        assert!(Mixer::new(0, 2).is_none());
        assert!(Mixer::new(2, 0).is_none());
    }

    #[test]
    fn mono_is_duplicated_into_stereo() {
        assert_eq!(mix(1, 2, &[0.5, -0.25]), [0.5, 0.5, -0.25, -0.25]);
    }

    #[test]
    fn stereo_is_averaged_into_mono() {
        assert_eq!(mix(2, 1, &[1.0, 0.0, 0.5, 0.5]), [0.5, 0.5]);
    }

    #[test]
    fn surround_downmix_drops_lfe_and_never_clips() {
        // 5.1 frame: only the LFE is loud → silence after the downmix.
        let out = mix(6, 2, &[0.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
        assert_eq!(out, [0.0, 0.0]);
        // Full-scale everywhere else stays within full scale.
        let out = mix(6, 2, &[1.0, 1.0, 1.0, 0.0, 1.0, 1.0]);
        assert!(out.iter().all(|s| (s - 1.0).abs() < 1e-5), "{out:?}");
    }

    #[test]
    fn surround_to_mono_is_the_average_of_the_stereo_downmix() {
        // Left-only front channel: stereo = (l, 0) -> mono = l / 2.
        let stereo = mix(6, 2, &[1.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        let mono = mix(6, 1, &[1.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        assert!((mono[0] - 0.5 * (stereo[0] + stereo[1])).abs() < 1e-6);
    }

    #[test]
    fn output_length_matches_the_frame_count() {
        let input = vec![0.1; 8 * 10];
        assert_eq!(mix(8, 2, &input).len(), 20);
        assert_eq!(mix(8, 1, &input).len(), 10);
        assert_eq!(mix(3, 2, &input[..9]).len(), 6);
    }
}
