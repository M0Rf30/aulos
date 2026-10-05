// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! cpal device resolution: a layered "configured → exact/substring match →
//! default → re-resolve-on-busy" fallback chain, plus DoP's stricter
//! `hw:`-only device search.
//!
//! `configured_device` is currently a stub that always returns `None` since
//! lyra has no output-device-selection setting today. The resolution
//! functions still accept the configured device as a plain `Option<&str>`
//! parameter so wiring a real "preferred output device" setting later is a
//! one-line change at the call sites, not a rewrite of the algorithm.

use crate::player::backend::PlayerError;
use cpal::traits::{DeviceTrait, HostTrait};
use cpal::{Device, SampleFormat, SampleRate, StreamConfig};

pub type Result<T, E = PlayerError> = std::result::Result<T, E>;

/// The output device configured via lyra's settings, if any. Currently
/// always `None` — lyra has no output-device-selection setting today. Kept
/// as its own function (rather than inlining `None` at each call site) so
/// wiring a real setting later only needs to change this one function.
pub fn configured_device() -> Option<String> {
    None
}

/// Whether an explicit output device is configured. Used to decide auto-DoP:
/// a dedicated device implies a real (likely DoP-capable) DAC.
pub fn output_device_configured() -> bool {
    configured_device().is_some()
}

/// Expand the classic ALSA shorthand `hw:<card>,<dev>` / `hw:<card>` (and the
/// `plughw:` variants) to cpal's enumerated `hw:CARD=<card>,DEV=<dev>` form, so
/// a configured value of `hw:1,0` matches the enumerated id `hw:CARD=1,DEV=0`.
/// Returns `None` when the input is not in numeric shorthand form.
fn normalize_alsa(name: &str) -> Option<String> {
    for prefix in ["plughw:", "hw:"] {
        if let Some(rest) = name.strip_prefix(prefix) {
            if rest.starts_with("CARD=") {
                return None; // already canonical
            }
            let mut parts = rest.splitn(2, ',');
            let card = parts.next()?.trim();
            let dev = parts
                .next()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("0");
            if card.is_empty()
                || !card.bytes().all(|b| b.is_ascii_digit())
                || !dev.bytes().all(|b| b.is_ascii_digit())
            {
                return None;
            }
            return Some(format!("{prefix}CARD={card},DEV={dev}"));
        }
    }
    None
}

/// CPAL device configuration helper.
pub struct CpalDeviceConfig {
    pub device: Device,
    pub config: StreamConfig,
    pub sample_format: SampleFormat,
}

/// Resolve the output device, honoring an explicitly configured device id.
///
/// When `configured` is set, select the output device whose **id** (the ALSA
/// PCM name, e.g. `hw:CARD=1,DEV=0`) or description matches it — exact match
/// first, then normalized-ALSA-shorthand, then exact description, then
/// case-insensitive substring on id, then case-insensitive substring on
/// description. This lets a raw ALSA hardware device be targeted to bypass
/// PipeWire/PulseAudio, which is required for bit-perfect DoP/DSD output (any
/// resampling, mixing, or volume change corrupts it).
fn resolve_output_device(host: &cpal::Host, configured: Option<&str>) -> Result<Device> {
    // (device, id string e.g. "hw:CARD=1,DEV=0", human description)
    let devices: Vec<(Device, String, String)> = host
        .output_devices()
        .map(|devs| {
            devs.map(|d| {
                let id = d.id().map(|i| i.id().to_owned()).unwrap_or_default();
                let desc = d.to_string();
                (d, id, desc)
            })
            .collect()
        })
        .unwrap_or_default();

    if let Some(want) = configured {
        // 1) exact id, 2) exact desc, 3) substring id, 4) substring desc
        let lower = want.to_lowercase();
        let norm = normalize_alsa(want);
        let pick = devices
            .iter()
            .find(|(_, id, _)| id == want)
            .or_else(|| {
                norm.as_deref()
                    .and_then(|n| devices.iter().find(|(_, id, _)| id == n))
            })
            .or_else(|| devices.iter().find(|(_, _, desc)| desc == want))
            .or_else(|| {
                devices
                    .iter()
                    .find(|(_, id, _)| id.to_lowercase().contains(&lower))
            })
            .or_else(|| {
                devices
                    .iter()
                    .find(|(_, _, desc)| desc.to_lowercase().contains(&lower))
            });
        if let Some((dev, id, desc)) = pick {
            tracing::info!("using output device id='{id}' desc='{desc}' (configured '{want}')");
            return Ok(dev.clone());
        }
        let available: Vec<&str> = devices.iter().map(|(_, id, _)| id.as_str()).collect();
        tracing::warn!(
            "configured output device '{want}' not found; using default. Available ids: {available:?}"
        );
    }

    host.default_output_device()
        .ok_or_else(|| PlayerError("No output device available".to_owned()))
}

/// Find a bit-perfect output device for DoP/native DSD: a raw ALSA `hw:` device
/// that natively supports the exact `rate` with at least `channels` channels, so
/// PipeWire/PulseAudio never resamples and corrupts the DoP stream.
///
/// HDMI and S/PDIF outputs are excluded — they advertise high PCM rates but are
/// not DoP DACs (auto-picking one yields silence). USB-described devices are
/// preferred, then the highest maximum rate. Returns `None` when nothing clearly
/// qualifies; the caller should fall back to PCM rather than guess. For
/// certainty (or when the DAC is briefly busy), configure the device explicitly.
fn find_dop_device(host: &cpal::Host, rate: SampleRate, channels: u16) -> Option<(Device, String)> {
    // (device, id, is_usb, max_rate)
    let mut best: Option<(Device, String, bool, u32)> = None;
    for device in host.output_devices().ok()? {
        let id = device.id().map(|i| i.id().to_owned()).unwrap_or_default();
        // Only raw hardware devices give an exclusive, non-resampled path.
        if !id.starts_with("hw:") {
            continue;
        }
        let desc = device.to_string().to_lowercase();
        // HDMI/SPDIF take high PCM rates but are not DoP DACs — never auto-pick.
        if desc.contains("hdmi") || desc.contains("s/pdif") || desc.contains("iec958") {
            continue;
        }
        let mut supports = false;
        let mut max_rate = 0u32;
        if let Ok(configs) = device.supported_output_configs() {
            for c in configs {
                max_rate = max_rate.max(c.max_sample_rate());
                if c.channels() >= channels
                    && rate >= c.min_sample_rate()
                    && rate <= c.max_sample_rate()
                {
                    supports = true;
                }
            }
        }
        if !supports {
            continue;
        }
        let is_usb = desc.contains("usb");
        // Prefer a USB DAC, then the highest maximum rate.
        if best
            .as_ref()
            .is_none_or(|(_, _, u, m)| (is_usb, max_rate) > (*u, *m))
        {
            best = Some((device, id, is_usb, max_rate));
        }
    }
    best.map(|(device, id, _, _)| (device, id))
}

impl CpalDeviceConfig {
    /// Create a new device configuration with the given sample rate and channels.
    pub fn new(sample_rate: SampleRate, channels: u16) -> Result<Self> {
        let host = cpal::default_host();
        let mut device = resolve_output_device(&host, configured_device().as_deref())?;

        // The configured device may be busy (held by PipeWire) or disconnected.
        // For ordinary PCM playback, degrade gracefully to the default device
        // rather than failing all playback. (DoP uses `new_dop`, which instead
        // reports the failure so the engine falls back to PCM conversion.)
        if device.supported_output_configs().is_err()
            && let Some(default) = host.default_output_device()
        {
            tracing::warn!(
                "configured output device '{device}' is unavailable/busy; \
                 falling back to the default device"
            );
            device = default;
        }
        tracing::debug!("cpal output device: {device}");

        // Choose a rate the device actually supports: the requested rate when
        // available, otherwise the device's default rate (always supported).
        // When they differ the caller resamples to bridge the gap, so playback
        // never fails just because the exact rate is unsupported.
        let rate = Self::device_supported_rate(&device, sample_rate);

        let config = StreamConfig {
            channels,
            sample_rate: rate,
            buffer_size: cpal::BufferSize::Default,
        };

        Ok(Self {
            device,
            config,
            sample_format: SampleFormat::F32, // Default, will be updated by find methods
        })
    }

    /// Device configuration for DoP/native DSD at the **exact** `sample_rate`
    /// (no resampling). Auto-selects a bit-perfect `hw:` DAC that natively
    /// supports the rate (`find_dop_device`), preferring an explicitly
    /// configured device when it qualifies, and falling back to the resolved
    /// device otherwise (DoP then likely fails and the caller reverts to PCM).
    pub fn new_dop(sample_rate: SampleRate, channels: u16) -> Result<Self> {
        let host = cpal::default_host();
        let configured = configured_device();
        // An explicitly configured device is used verbatim (no auto-substitution),
        // so DoP never silently routes to the wrong output. Auto-discovery only
        // runs when no device is configured, and only over real (USB) DACs.
        let device = if configured.is_some() {
            let dev = resolve_output_device(&host, configured.as_deref())?;
            tracing::info!("DoP: using configured device '{dev}' at {sample_rate} Hz");
            dev
        } else {
            match find_dop_device(&host, sample_rate, channels) {
                Some((dev, id)) => {
                    tracing::info!(
                        "DoP: auto-selected bit-perfect device '{id}' at {sample_rate} Hz"
                    );
                    dev
                }
                // No real DAC: fail so the caller cleanly reverts to PCM, rather
                // than opening DoP on a wrong/HDMI/PipeWire device (silence).
                None => {
                    return Err(PlayerError(format!(
                        "no USB/hardware DAC natively supports {sample_rate} Hz \
                         (HDMI/SPDIF excluded); configure an explicit output device"
                    )));
                }
            }
        };

        let config = StreamConfig {
            channels,
            sample_rate,
            buffer_size: cpal::BufferSize::Default,
        };

        Ok(Self {
            device,
            config,
            sample_format: SampleFormat::F32,
        })
    }

    /// Return `requested` if the device supports it, else the device's default
    /// output rate (which is always supported by definition).
    fn device_supported_rate(device: &Device, requested: SampleRate) -> SampleRate {
        if Self::device_supports(device, requested) {
            return requested;
        }
        device
            .default_output_config()
            .map(|c| c.sample_rate())
            .unwrap_or(requested)
    }

    /// Whether `device` advertises support for `rate`.
    fn device_supports(device: &Device, rate: SampleRate) -> bool {
        device
            .supported_output_configs()
            .map(|configs| {
                configs
                    .into_iter()
                    .any(|c| rate >= c.min_sample_rate() && rate <= c.max_sample_rate())
            })
            .unwrap_or(false)
    }

    /// Whether the default output device natively supports `rate` (no
    /// resampling required). Used to prefer bit-exact rates.
    pub fn default_device_supports_rate(rate: SampleRate) -> bool {
        let host = cpal::default_host();
        resolve_output_device(&host, configured_device().as_deref())
            .map(|device| Self::device_supports(&device, rate))
            .unwrap_or(false)
    }

    /// The default output device's preferred (default) sample rate in Hz, if
    /// known. Used to size DSD-to-PCM decoding to the device instead of to the
    /// (often huge) advertised maximum.
    pub fn default_output_rate() -> Option<SampleRate> {
        let host = cpal::default_host();
        resolve_output_device(&host, configured_device().as_deref())
            .ok()
            .and_then(|device| device.default_output_config().ok())
            .map(|config| config.sample_rate())
    }

    /// Find the best PCM format (prefers F32, then I16, then I32).
    pub fn find_pcm_format(&mut self) -> Result<SampleFormat> {
        let preferences = &[SampleFormat::F32, SampleFormat::I16, SampleFormat::I32];
        self.find_format_with_preference(preferences, "PCM")
    }

    /// Find the best DoP format (prefers I24, then I32).
    pub fn find_dop_format(&mut self) -> Result<SampleFormat> {
        let preferences = &[SampleFormat::I24, SampleFormat::I32];
        self.find_format_with_preference(preferences, "DoP")
    }

    /// Find format matching the given preferences, always choosing the
    /// highest-preference format the device supports regardless of enumeration
    /// order. Tracks the best preference index seen so far and upgrades
    /// `found_format` whenever a higher-priority (lower index) format appears.
    fn find_format_with_preference(
        &mut self,
        preferences: &[SampleFormat],
        format_type: &str,
    ) -> Result<SampleFormat> {
        let supported_configs = self
            .device
            .supported_output_configs()
            .map_err(|e| PlayerError(format!("Failed to get supported configs: {e}")))?;

        let mut found_format: Option<SampleFormat> = None;
        // Sentinel: preferences.len() means "no match yet"; lower index = higher priority.
        let mut best_idx: usize = preferences.len();

        tracing::debug!(
            "searching for suitable {} format at {:?} Hz",
            format_type,
            self.config.sample_rate
        );

        for config in supported_configs {
            let sample_format = config.sample_format();
            let min_rate = config.min_sample_rate();
            let max_rate = config.max_sample_rate();

            if self.config.sample_rate >= min_rate && self.config.sample_rate <= max_rate {
                // Walk the preference list to find this format's rank.
                for (i, &preferred_format) in preferences.iter().enumerate() {
                    if sample_format == preferred_format && i < best_idx {
                        // Higher-priority (or first) match — upgrade.
                        found_format = Some(sample_format);
                        best_idx = i;
                        tracing::debug!(
                            "found {} format (preference {}): {:?} at {:?}-{:?} Hz",
                            format_type,
                            i,
                            sample_format,
                            min_rate,
                            max_rate
                        );
                        break; // no need to check lower-priority preferences for this config
                    }
                }

                // Top preference found — stop searching device configs entirely.
                if best_idx == 0 {
                    break;
                }
            }
        }

        let format = found_format.unwrap_or(preferences[0]);
        tracing::debug!("using {} sample format: {:?}", format_type, format);

        self.sample_format = format;
        Ok(format)
    }
}
