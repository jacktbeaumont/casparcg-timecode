//! Timecode parsing and event detection.

use std::collections::VecDeque;
use std::time::{Duration, Instant};
use timecode_coder::ltc_decoder::LtcDecoder;
use timecode_coder::{FramesPerSecond, TimecodeFrame};

/// A timecode position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimecodePosition {
    /// The underlying LTC timecode frame.
    pub frame: TimecodeFrame,
    /// The framerate of this timecode.
    pub fps: u8,
    /// Absolute frame position on the timecode timeline.
    pub total_frames: u32,
}

impl TimecodePosition {
    /// Resolves a raw [`TimecodeFrame`], substituting `fallback_fps` when the
    /// signal reports [`FramesPerSecond::Unknown`].
    pub fn new(frame: TimecodeFrame, fallback_fps: u8) -> Self {
        let fps = match frame.frames_per_second {
            FramesPerSecond::TwentyFour => 24,
            FramesPerSecond::TwentyFive => 25,
            FramesPerSecond::Thirty => 30,
            FramesPerSecond::Unknown => {
                tracing::warn!(
                    "LTC signal reports unknown framerate; falling back to {}fps",
                    fallback_fps
                );
                fallback_fps
            }
        };
        let h = frame.hours as u32;
        let m = frame.minutes as u32;
        let s = frame.seconds as u32;
        let f = frame.frames as u32;
        let total_frames = (h * 3600 + m * 60 + s) * fps as u32 + f;
        Self {
            frame,
            fps,
            total_frames,
        }
    }
}

impl std::fmt::Display for TimecodePosition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:02}:{:02}:{:02}:{:02}",
            self.frame.hours, self.frame.minutes, self.frame.seconds, self.frame.frames
        )
    }
}

/// A timecode state change event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimecodeEvent {
    /// Timecode playing with frame.
    Playing(TimecodePosition),
    /// Timecode paused at frame.
    Paused(TimecodePosition),
}

#[derive(Default)]
enum PlayState {
    #[default]
    Idle,
    Playing {
        tc: TimecodeFrame,
        received_at: Instant,
    },
    Paused {
        tc: TimecodeFrame,
    },
}

/// An LTC audio decoder that detects timecode playback events.
pub struct TimecodeParser {
    decoder: LtcDecoder<i16>,
    state: PlayState,
    pause_timeout: Duration,
    pending_events: VecDeque<TimecodeEvent>,
    /// Framerate used when the LTC signal reports `Unknown`.
    fallback_fps: u8,
}

impl TimecodeParser {
    pub fn new(audio_sample_rate: u32, pause_timeout_ms: u64, fallback_fps: u8) -> Self {
        Self {
            decoder: LtcDecoder::new(audio_sample_rate),
            state: PlayState::Idle,
            pause_timeout: Duration::from_millis(pause_timeout_ms),
            pending_events: VecDeque::new(),
            fallback_fps,
        }
    }

    /// Feeds a chunk of mono PCM audio into the LTC decoder.
    ///
    /// `samples` must be de-interleaved to a single channel and normalised to `[-1.0, 1.0]`.
    /// Decoded timecode frames are queued internally and returned by [`next`](Self::next).
    /// `now` records the arrival time of this chunk for pause-timeout detection.
    pub fn push(&mut self, samples: &[f32], now: Instant) {
        for &sample in samples {
            let sample_i16 = (sample * i16::MAX as f32) as i16;
            if let Some(tc) = self.decoder.get_timecode_frame(sample_i16) {
                let last_tc = match &self.state {
                    PlayState::Playing { tc, .. } | PlayState::Paused { tc } => Some(tc),
                    PlayState::Idle => None,
                };
                if last_tc == Some(&tc) {
                    continue;
                }
                self.state = PlayState::Playing {
                    tc: tc.clone(),
                    received_at: now,
                };
                let pos = TimecodePosition::new(tc, self.fallback_fps);
                self.pending_events.push_back(TimecodeEvent::Playing(pos));
            }
        }
    }

    /// Returns the next decoded timecode event, or `None` if none are pending.
    ///
    /// When the event queue is empty, checks whether the pause timeout has elapsed since the
    /// last decoded frame and emits [`TimecodeEvent::Paused`] if so. Pass the same `now` as
    /// the preceding [`push`](Self::push) call to ensure consistent timeout evaluation within
    /// a single poll cycle.
    pub fn next(&mut self, now: Instant) -> Option<TimecodeEvent> {
        self.pending_events
            .pop_front()
            .or_else(|| self.check_timeout(now))
    }

    fn check_timeout(&mut self, now: Instant) -> Option<TimecodeEvent> {
        if let PlayState::Playing { tc, received_at } = &self.state
            && now.duration_since(*received_at) >= self.pause_timeout
        {
            let pos = TimecodePosition::new(tc.clone(), self.fallback_fps);
            let event = TimecodeEvent::Paused(pos);
            self.state = PlayState::Paused { tc: tc.clone() };
            return Some(event);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use timecode_coder::FramesPerSecond;

    fn tc(h: u8, m: u8, s: u8, f: u8) -> TimecodeFrame {
        TimecodeFrame::new(h, m, s, f, FramesPerSecond::TwentyFive)
    }

    fn pos(h: u8, m: u8, s: u8, f: u8) -> TimecodePosition {
        TimecodePosition::new(tc(h, m, s, f), 25)
    }

    fn test_parser() -> TimecodeParser {
        TimecodeParser::new(48000, 200, 25)
    }

    #[test]
    fn timeout_produces_paused_once() {
        let mut parser = test_parser();
        let t0 = Instant::now();

        parser.state = PlayState::Playing {
            tc: tc(1, 0, 0, 5),
            received_at: t0,
        };

        assert_eq!(parser.next(t0 + Duration::from_millis(100)), None);
        assert_eq!(
            parser.next(t0 + Duration::from_millis(250)),
            Some(TimecodeEvent::Paused(pos(1, 0, 0, 5))),
        );
        assert_eq!(parser.next(t0 + Duration::from_millis(300)), None);
    }
}
