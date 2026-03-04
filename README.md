# casparcg-timecode

Triggers [CasparCG Server](https://casparcg.com/) media playback from an incoming LTC audio signal. Decodes LTC from an audio input, maps timecodes to configured media cues, and issues AMCP commands to start, seek, pause, and resume playback on the server.

## Requirements

- Rust 1.93.1+ (2024 edition)
- CasparCG Server with AMCP enabled
- Audio input carrying LTC (hardware interface or virtual device such as BlackHole)

## Installation

```sh
git clone https://github.com/jacktbeaumont/casparcg-timecode.git
cd casparcg-timecode
cargo build --release
```

## Usage

```sh
casparcg-timecode                        # run with default config.yaml
casparcg-timecode --config show.yaml     # custom config
casparcg-timecode --list-devices         # list audio inputs
```

## Tracks and layers

A track maps a start timecode (`tc_start`) to a set of media items. Each item targets a CasparCG layer on the configured channel. Overlapping layer assignments at the same timecode position are rejected at startup.

```yaml
tracks:
  - name: "Background"
    tc_start: "01:00:00:00"
    media:
      - layer: 10
        file: "BG_LOOP"
  - name: "Feature"
    tc_start: "01:05:00:00"
    media:
      - layer: 20
        file: "MAIN_FILM"
      - layer: 30
        file: "LOWER_THIRD"
```

## Configuration

Copy and edit the example config:

```sh
cp config.example.yaml config.yaml
```
