//! Config file loading and type definition.

use anyhow::{Context, Result};
use serde::{Deserialize, Deserializer};
use std::{fs, ops::Deref};

/// CasparCG layer number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LayerId(u16);

impl LayerId {
    #[cfg(test)]
    pub fn new(id: u16) -> Self {
        Self(id)
    }
}

impl std::fmt::Display for LayerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl Deref for LayerId {
    type Target = u16;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'de> Deserialize<'de> for LayerId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let num = u16::deserialize(deserializer)?;
        Ok(LayerId(num))
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Timecode {
    pub hours: u8,
    pub minutes: u8,
    pub seconds: u8,
    pub frames: u8,
}

impl Timecode {
    /// Converts this timecode to total frames using the given framerate.
    pub fn total_frames(&self, fps: f32) -> u32 {
        let h: u32 = self.hours.into();
        let m: u32 = self.minutes.into();
        let s: u32 = self.seconds.into();
        let f: u32 = self.frames.into();
        ((h * 3600 + m * 60 + s) as f32 * fps) as u32 + f
    }
}

impl TryFrom<&str> for Timecode {
    type Error = anyhow::Error;

    fn try_from(s: &str) -> Result<Self> {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() != 4 {
            anyhow::bail!("invalid timecode format: {}", s);
        }
        let h: u8 = parts[0].parse()?;
        let m: u8 = parts[1].parse()?;
        let sec: u8 = parts[2].parse()?;
        let f: u8 = parts[3].parse()?;
        if m >= 60 {
            anyhow::bail!("invalid minutes in timecode: {}", s);
        }
        if sec >= 60 {
            anyhow::bail!("invalid seconds in timecode: {}", s);
        }
        Ok(Timecode {
            hours: h,
            minutes: m,
            seconds: sec,
            frames: f,
        })
    }
}

fn deserialize_timecode<'de, D>(deserializer: D) -> std::result::Result<Timecode, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    Timecode::try_from(s.as_str()).map_err(serde::de::Error::custom)
}

fn default_resync_threshold_frames() -> u32 {
    10
}

fn default_tc_fallback_fps() -> u8 {
    25
}

fn default_tcp_timeout_secs() -> u64 {
    5
}

/// Media layer configuration for a track
#[derive(Debug, Clone, Deserialize)]
pub struct MediaLayer {
    /// CasparCG layer number
    pub layer: LayerId,
    /// File path to play
    pub file: String,
}

/// Track definition mapping a timecode to media files
#[derive(Debug, Clone, Deserialize)]
pub struct Track {
    /// Human-readable track name
    pub name: String,
    /// Starting timecode for this track (e.g., "01:10:00:00")
    /// When this timecode is received, the video starts at frame 0
    #[serde(deserialize_with = "deserialize_timecode")]
    pub tc_start: Timecode,
    /// Media layers to play for this track
    pub media: Vec<MediaLayer>,
}

/// Application configuration
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// CasparCG server hostname or IP address
    pub caspar_host: String,
    /// CasparCG AMCP port (usually 5250)
    pub caspar_port: u16,
    /// CasparCG channel to control
    pub caspar_channel: u16,
    /// Audio device name for LTC input (None = default device)
    pub audio_device: Option<String>,
    /// Milliseconds without a new LTC frame before playback is considered paused
    pub pause_detection_threshold_ms: u64,
    /// Frame difference above which a timecode jump triggers a resync. Default is 10 frames.
    #[serde(default = "default_resync_threshold_frames")]
    pub resync_threshold_frames: u32,
    /// Fallback framerate when timecode framerate is unknown. Default is 25 fps.
    #[serde(default = "default_tc_fallback_fps")]
    pub tc_fallback_fps: u8,
    /// Timeout in seconds for TCP connect and AMCP command I/O. Default is 5.
    #[serde(default = "default_tcp_timeout_secs")]
    pub tcp_timeout_secs: u64,
    /// Track definitions
    pub tracks: Vec<Track>,
}

impl Config {
    /// Load configuration from a YAML file
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the YAML configuration file
    pub fn from_file(path: &str) -> Result<Self> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("failed to read config file: {}", path))?;

        let config: Config = serde_yaml::from_str(&contents)
            .with_context(|| format!("failed to parse config file: {}", path))?;

        config.validate()?;

        Ok(config)
    }

    /// Validate the configuration, returning an error if any constraints are violated.
    pub(crate) fn validate(&self) -> Result<()> {
        use std::collections::HashSet;

        if self.tc_fallback_fps == 0 {
            anyhow::bail!("tc_fallback_fps must be greater than 0");
        }

        if self.tcp_timeout_secs == 0 {
            anyhow::bail!("tcp_timeout_secs must be greater than 0");
        }

        if self.pause_detection_threshold_ms < 10 {
            anyhow::bail!("pause_detection_threshold_ms must be at least 10");
        }

        for track in &self.tracks {
            let mut seen_layers: HashSet<u16> = HashSet::new();
            for media_layer in &track.media {
                if !seen_layers.insert(*media_layer.layer) {
                    anyhow::bail!(
                        "track '{}' has layer {} assigned to multiple files; \
                         a layer can only appear once per track",
                        track.name,
                        media_layer.layer,
                    );
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_config(tracks: Vec<Track>) -> Config {
        Config {
            caspar_host: "localhost".into(),
            caspar_port: 5250,
            caspar_channel: 1,
            audio_device: None,
            pause_detection_threshold_ms: 500,
            resync_threshold_frames: 10,
            tc_fallback_fps: 25,
            tcp_timeout_secs: 5,
            tracks,
        }
    }

    fn make_track(id: u16, media: Vec<(u16, &str)>) -> Track {
        Track {
            name: format!("track_{id}"),
            tc_start: Timecode {
                hours: 1,
                minutes: 0,
                seconds: 0,
                frames: 0,
            },
            media: media
                .into_iter()
                .map(|(layer, file)| MediaLayer {
                    layer: LayerId(layer),
                    file: file.into(),
                })
                .collect(),
        }
    }

    #[test]
    fn validate_accepts_unique_layers() {
        let config = make_config(vec![
            make_track(1, vec![(10, "a.mp4"), (20, "b.mp4")]),
            make_track(2, vec![(10, "c.mp4")]),
        ]);
        config.validate().unwrap();
    }

    #[test]
    fn validate_rejects_duplicate_layer() {
        let config = make_config(vec![
            make_track(1, vec![(10, "a.mp4")]),
            make_track(2, vec![(10, "x.mp4"), (10, "y.mp4")]),
        ]);
        let err = config.validate().unwrap_err().to_string();
        assert!(
            err.contains("track_2"),
            "error should name the offending track: {err}"
        );
        assert!(
            err.contains("layer 10"),
            "error should name the duplicate layer: {err}"
        );
    }
}
