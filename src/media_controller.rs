//! Media playback controller that responds to timecode events.

use crate::config::{Config, LayerId};
use crate::timecode_parser::{TimecodeEvent, TimecodePosition};
use crate::{amcp::AmcpClient, config::Timecode};
use anyhow::Result;
use std::collections::{HashMap, HashSet};

/// A media item to be played on a CasparCG layer, with associated timecode metadata.
#[derive(Debug, Clone)]
pub struct MediaItem {
    /// The CasparCG layer to play this media on.
    pub layer: LayerId,
    /// The media filename in CasparCG.
    pub filename: String,
    /// The timecode at which this media should start playing.
    pub start_tc: Timecode,
    /// The total duration of this media in frames.
    pub duration_frames: u32,
    /// The native framerate of this media is played at.
    pub fps: f32,
    /// Frames to start playback earlier, compensating for output delay.
    pub output_delay_frames: u32,
}

impl MediaItem {
    /// Start time in seconds, truncating the sub-second frames component.
    fn start_secs(&self) -> f64 {
        let tc = &self.start_tc;
        tc.hours as f64 * 3600.0 + tc.minutes as f64 * 60.0 + tc.seconds as f64
    }

    /// End time in seconds, rounded up to the next whole second.
    fn end_secs(&self) -> f64 {
        let tc = &self.start_tc;
        let mut start = self.start_secs();
        if tc.frames > 0 {
            start += 1.0; // worst-case
        }
        start + self.duration_frames as f64 / self.fps as f64
    }

    /// Returns the start frame of this media item in the timecode timeline at the given TC fps,
    /// shifted earlier by `output_delay_frames` to compensate for output latency.
    fn start_frame(&self, tc_fps: f32) -> u32 {
        self.start_tc
            .total_frames(tc_fps)
            .saturating_sub(self.output_delay_frames)
    }

    /// Returns the end frame (exclusive) of this media item at the given TC fps.
    fn end_frame(&self, tc_fps: f32) -> u32 {
        let duration_tc_frames = (self.duration_frames as f32 / self.fps * tc_fps).round() as u32;
        self.start_frame(tc_fps) + duration_tc_frames
    }

    /// Returns whether this media item should be active at the given timecode position.
    pub fn is_active_at(&self, pos: &TimecodePosition) -> bool {
        let tc_fps = pos.fps as f32;
        pos.total_frames >= self.start_frame(tc_fps) && pos.total_frames < self.end_frame(tc_fps)
    }

    /// Computes the server frame to SEEK to for the given timecode position.
    fn media_offset(&self, pos: &TimecodePosition, server_fps: f32) -> u32 {
        let tc_fps = pos.fps as f32;
        let timecode_offset = pos.total_frames - self.start_frame(tc_fps);
        (timecode_offset as f32 * server_fps / tc_fps).round() as u32
    }
}

/// Playback state for a single layer.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) enum LayerState {
    #[default]
    Stopped,
    Playing {
        filename: String,
    },
    Paused {
        filename: String,
    },
}

/// Configuration and state for media playback control.
struct MediaConfig {
    /// List of all media items to manage.
    media_items: Vec<MediaItem>,
    /// CasparCG channel number to control.
    channel: u16,
    /// Frame-count threshold beyond which a timecode jump triggers a full resync.
    resync_threshold_frames: u32,
    /// Output frame rate of the CasparCG channel (used for SEEK calculations).
    server_fps: f32,
}

/// Controller that manages media playback on CasparCG in response to timecode events.
pub struct MediaController {
    config: MediaConfig,
    /// Current playback state of each CasparCG layer, keyed by layer number.
    layer_states: HashMap<LayerId, LayerState>,
    /// AMCP client for sending commands to CasparCG.
    amcp: AmcpClient,
    /// The previous timecode frame, used for skip detection.
    last_tc_frame: Option<u32>,
    /// Whether timecode was playing (vs paused) on the previous event.
    was_playing: bool,
}

impl MediaController {
    /// Creates a new `MediaController`, validating all media files via AMCP.
    pub async fn new(config: &Config, mut amcp: AmcpClient) -> Result<Self> {
        let server_fps = amcp.channel_fps(config.caspar_channel).await?;
        tracing::info!(
            "server channel {} fps: {}",
            config.caspar_channel,
            server_fps
        );

        let media_info = Self::load_media(&mut amcp, config).await?;

        let mut media_items = Vec::new();
        let mut layer_states = HashMap::new();

        for track in &config.tracks {
            for layer_cfg in &track.media {
                let info = media_info.get(&layer_cfg.file).ok_or_else(|| {
                    anyhow::anyhow!("media info not found for: {}", layer_cfg.file)
                })?;

                if info.frame_rate <= 0.0 {
                    anyhow::bail!(
                        "media '{}' has invalid frame rate: {}",
                        layer_cfg.file,
                        info.frame_rate
                    );
                }

                let item = MediaItem {
                    layer: layer_cfg.layer,
                    filename: layer_cfg.file.clone(),
                    start_tc: track.tc_start.clone(),
                    duration_frames: info.frame_count,
                    fps: info.frame_rate,
                    output_delay_frames: config.output_delay_frames,
                };

                media_items.push(item);
                layer_states.entry(layer_cfg.layer).or_default();
            }
        }

        Self::check_layer_conflicts(&media_items)?;

        Ok(Self {
            config: MediaConfig {
                media_items,
                channel: config.caspar_channel,
                resync_threshold_frames: config.resync_threshold_frames,
                server_fps,
            },
            layer_states,
            amcp,
            last_tc_frame: None,
            was_playing: false,
        })
    }

    /// Returns an error if any two media items share a layer and have overlapping active ranges.
    fn check_layer_conflicts(media_items: &[MediaItem]) -> Result<()> {
        let mut by_layer: HashMap<LayerId, Vec<usize>> = HashMap::new();
        for (i, item) in media_items.iter().enumerate() {
            by_layer.entry(item.layer).or_default().push(i);
        }

        for (layer, indices) in &by_layer {
            for i in 0..indices.len() {
                for j in (i + 1)..indices.len() {
                    let a = &media_items[indices[i]];
                    let b = &media_items[indices[j]];

                    let a_start = a.start_secs();
                    let a_end = a.end_secs();
                    let b_start = b.start_secs();
                    let b_end = b.end_secs();

                    if a_start < b_end && b_start < a_end {
                        anyhow::bail!(
                            "layer {} has a scheduling conflict: '{}' (active {:.1}s–{:.1}s) \
                             overlaps with '{}' (active {:.1}s–{:.1}s)",
                            layer,
                            a.filename,
                            a_start,
                            a_end,
                            b.filename,
                            b_start,
                            b_end,
                        );
                    }
                }
            }
        }

        Ok(())
    }

    /// Fetch and validate all media files referenced in `config` from CasparCG.
    async fn load_media(
        amcp: &mut AmcpClient,
        config: &Config,
    ) -> Result<HashMap<String, crate::amcp::MediaInfo>> {
        let mut media_info = HashMap::new();

        let filenames: HashSet<&str> = config
            .tracks
            .iter()
            .flat_map(|t| t.media.iter().map(|m| m.file.as_str()))
            .collect();

        for filename in filenames {
            match amcp.cinf(filename).await {
                Ok(info) => {
                    tracing::debug!(
                        "validated media '{}': {:?}, {} frames, {:.2} fps",
                        filename,
                        info.media_type,
                        info.frame_count,
                        info.frame_rate
                    );
                    media_info.insert(filename.to_string(), info);
                }
                Err(e) => {
                    return Err(anyhow::anyhow!(
                        "media file '{}' not found in CasparCG: {}",
                        filename,
                        e
                    ));
                }
            }
        }

        Ok(media_info)
    }

    /// Handles a timecode event and sends appropriate AMCP commands.
    pub async fn handle_event(&mut self, event: &TimecodeEvent) -> Result<()> {
        match event {
            TimecodeEvent::Playing(pos) => self.handle_playing(pos).await,
            TimecodeEvent::Paused(pos) => self.handle_paused(pos).await,
        }
    }

    /// Handles timecode playing.
    ///
    /// If a timecode jump is detected (beyond the configured threshold) or timecode was
    /// previously paused performs a full sync of all layers. Otherwise, performs a partial
    /// sync that only starts new media.
    async fn handle_playing(&mut self, pos: &TimecodePosition) -> Result<()> {
        let targets = self.compute_targets(pos);

        let skipped = match self.last_tc_frame {
            Some(prev) => {
                let delta = pos.total_frames.abs_diff(prev);
                delta > self.config.resync_threshold_frames
            }
            None => true, // First frame, treat as skip to do initial sync
        };

        if skipped || !self.was_playing {
            self.full_sync(&targets, pos).await?;
        } else {
            self.start_new_media(&targets, pos).await?;
        }

        self.last_tc_frame = Some(pos.total_frames);
        self.was_playing = true;
        Ok(())
    }

    /// Handles a pause event by pausing all currently playing layers.
    async fn handle_paused(&mut self, pos: &TimecodePosition) -> Result<()> {
        tracing::info!("pause at {}", pos);
        for (layer, state) in &mut self.layer_states {
            if let LayerState::Playing { filename } = state {
                tracing::info!("pausing layer {} ({})", layer, filename);
                if let Err(e) = self.amcp.pause(self.config.channel, **layer).await {
                    tracing::error!("failed to pause layer {}: {}", layer, e);
                    continue;
                }
                *state = LayerState::Paused {
                    filename: filename.clone(),
                };
            }
        }
        self.last_tc_frame = Some(pos.total_frames);
        self.was_playing = false;
        Ok(())
    }

    /// Determines which media (if any) should be playing on each layer at the given timecode.
    ///
    /// Returns a map of layer to optional index into [`MediaConfig::media_items`].
    fn compute_targets(&self, pos: &TimecodePosition) -> HashMap<LayerId, Option<usize>> {
        let mut targets: HashMap<LayerId, Option<usize>> = HashMap::new();
        for layer in self.layer_states.keys() {
            targets.insert(*layer, None);
        }

        for (i, media) in self.config.media_items.iter().enumerate() {
            if media.is_active_at(pos) {
                targets.insert(media.layer, Some(i));
            }
        }

        targets
    }

    /// Performs a full synchronisation of all layers.
    ///
    /// For each layer, stops playback if nothing should be playing, or issues a
    /// play/seek command if a file should be playing.
    async fn full_sync(
        &mut self,
        targets: &HashMap<LayerId, Option<usize>>,
        pos: &TimecodePosition,
    ) -> Result<()> {
        for (&layer, target) in targets {
            let state = self.layer_states.entry(layer).or_default();
            match target {
                Some(idx) => {
                    let media: &MediaItem = &self.config.media_items[*idx];
                    let offset = media.media_offset(pos, self.config.server_fps);
                    tracing::info!(
                        channel = self.config.channel,
                        layer = *layer,
                        filename = %media.filename,
                        seek_frame = offset,
                        prior_state = ?*state,
                        "seeking layer to target frame",
                    );
                    if let Err(e) = self
                        .amcp
                        .play(self.config.channel, *layer, &media.filename, Some(offset))
                        .await
                    {
                        tracing::error!("failed to play on layer {}: {}", layer, e);
                        continue;
                    }
                    *state = LayerState::Playing {
                        filename: media.filename.clone(),
                    };
                }
                None => {
                    if !matches!(state, LayerState::Stopped) {
                        tracing::info!("stopping layer {}", layer);
                        if let Err(e) = self.amcp.stop(self.config.channel, *layer).await {
                            tracing::error!("failed to stop layer {}: {}", layer, e);
                            continue;
                        }
                        *state = LayerState::Stopped;
                    }
                }
            }
        }
        Ok(())
    }

    /// Returns the current layer states, keyed by layer id.
    pub fn layer_states(&self) -> &HashMap<LayerId, LayerState> {
        &self.layer_states
    }

    /// Performs an incremental sync during normal playback.
    ///
    /// Only starts media on layers that are currently stopped but should be
    /// playing. Already-playing layers are left untouched.
    async fn start_new_media(
        &mut self,
        targets: &HashMap<LayerId, Option<usize>>,
        pos: &TimecodePosition,
    ) -> Result<()> {
        for (&layer, target) in targets {
            let state = self.layer_states.entry(layer).or_default();
            match target {
                Some(idx) => {
                    if matches!(state, LayerState::Stopped) {
                        let media = &self.config.media_items[*idx];
                        let offset = media.media_offset(pos, self.config.server_fps);
                        tracing::info!(
                            channel = self.config.channel,
                            layer = *layer,
                            filename = %media.filename,
                            seek_frame = offset,
                            "starting new media on layer",
                        );
                        if let Err(e) = self
                            .amcp
                            .play(self.config.channel, *layer, &media.filename, Some(offset))
                            .await
                        {
                            tracing::error!("failed to play on layer {}: {}", layer, e);
                            continue;
                        }
                        *state = LayerState::Playing {
                            filename: media.filename.clone(),
                        };
                    }
                }
                None => {
                    if !matches!(state, LayerState::Stopped) {
                        tracing::info!("stopping layer {} (media ended)", layer);
                        if let Err(e) = self.amcp.stop(self.config.channel, *layer).await {
                            tracing::error!("failed to stop layer {}: {}", layer, e);
                            continue;
                        }
                        *state = LayerState::Stopped;
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LayerId, Timecode};

    fn media(
        layer: u16,
        file: &str,
        tc_start: [u8; 4],
        duration_frames: u32,
        fps: f32,
    ) -> MediaItem {
        MediaItem {
            layer: LayerId::new(layer),
            filename: file.into(),
            start_tc: Timecode {
                hours: tc_start[0],
                minutes: tc_start[1],
                seconds: tc_start[2],
                frames: tc_start[3],
            },
            duration_frames,
            fps,
            output_delay_frames: 0,
        }
    }

    #[test]
    fn check_layer_conflicts_accepts_non_overlapping() {
        let items = vec![
            media(10, "a.mp4", [1, 0, 0, 0], 50, 25.0),
            media(10, "b.mp4", [1, 0, 5, 0], 50, 25.0),
        ];
        MediaController::check_layer_conflicts(&items)
            .expect("sequential items on the same layer should not conflict");
    }

    #[test]
    fn check_layer_conflicts_accepts_different_layers() {
        let items = vec![
            media(10, "a.mp4", [1, 0, 0, 0], 50, 25.0),
            media(20, "b.mp4", [1, 0, 0, 0], 50, 25.0),
        ];
        MediaController::check_layer_conflicts(&items)
            .expect("overlapping items on different layers should not conflict");
    }

    #[test]
    fn check_layer_conflicts_rejects_overlapping() {
        let items = vec![
            media(10, "a.mp4", [1, 0, 0, 0], 100, 25.0),
            media(10, "b.mp4", [1, 0, 3, 0], 100, 25.0),
        ];
        let err = MediaController::check_layer_conflicts(&items)
            .expect_err("overlapping items on the same layer should conflict")
            .to_string();
        assert!(
            err.contains("scheduling conflict"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn check_layer_conflicts_rejects_identical_timing() {
        let items = vec![
            media(10, "a.mp4", [1, 0, 0, 0], 50, 25.0),
            media(10, "b.mp4", [1, 0, 0, 0], 50, 25.0),
        ];
        let err = MediaController::check_layer_conflicts(&items)
            .expect_err("identical timing on the same layer should conflict")
            .to_string();
        assert!(
            err.contains("scheduling conflict"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn output_delay_activates_media_earlier() {
        use crate::timecode_parser::TimecodePosition;

        let mut item = media(10, "a.mp4", [1, 0, 0, 0], 250, 25.0);
        item.output_delay_frames = 8;

        // At 25fps, tc_start "01:00:00:00" = 90000 frames.
        // With 8-frame delay, activation starts at 89992.
        let pos_before = TimecodePosition::test(89991, 25);
        let pos_at_delay = TimecodePosition::test(89992, 25);
        let pos_at_original = TimecodePosition::test(90000, 25);

        assert!(!item.is_active_at(&pos_before));
        assert!(item.is_active_at(&pos_at_delay));
        assert!(item.is_active_at(&pos_at_original));
    }

    #[test]
    fn output_delay_seek_offset_is_zero_at_adjusted_start() {
        let mut item = media(10, "a.mp4", [1, 0, 0, 0], 250, 25.0);
        item.output_delay_frames = 8;

        use crate::timecode_parser::TimecodePosition;
        let pos = TimecodePosition::test(89992, 25);

        // At the adjusted start, seek offset should be 0
        assert_eq!(item.media_offset(&pos, 25.0), 0);
    }
}
