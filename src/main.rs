mod amcp;
mod config;
mod media_controller;
mod timecode_parser;
mod tui;

use amcp::AmcpClient;
use anyhow::{Result, anyhow};
use clap::Parser;
use config::Config;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use media_controller::MediaController;
use std::time::{Duration, Instant};
use timecode_parser::{TimecodeEvent, TimecodeParser};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use tui::state::{self, TcStatus, UiMessage};

/// CasparCG Timecode Client
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// List available audio input devices and exit
    #[arg(short = 'l', long)]
    list_devices: bool,

    /// Config file path
    #[arg(short = 'c', long, default_value = "config.yaml")]
    config: String,
}

fn device_name(device: &cpal::Device) -> String {
    device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "Unknown".to_string())
}

fn list_audio_devices() -> Result<()> {
    let host = cpal::default_host();
    let default_name = host.default_input_device().map(|d| device_name(&d));

    println!("\nAvailable audio input devices:\n");
    for (i, device) in host.input_devices()?.enumerate() {
        let name = device_name(&device);
        let marker = if default_name.as_deref() == Some(&name) {
            " [DEFAULT]"
        } else {
            ""
        };
        println!("  {}. {}{}", i + 1, name, marker);
    }
    Ok(())
}

fn get_audio_device(config: &Config) -> Result<cpal::Device> {
    let host = cpal::default_host();
    match &config.audio_device {
        Some(name) => host
            .input_devices()?
            .find(|d| device_name(d) == name.as_str())
            .ok_or_else(|| anyhow!("audio device '{}' not found", name)),
        None => host
            .default_input_device()
            .ok_or_else(|| anyhow!("no default input device found")),
    }
}

async fn run(
    config: Config,
    ui_tx: mpsc::Sender<UiMessage>,
    token: CancellationToken,
) -> Result<()> {
    tracing::info!(
        "connecting to CasparCG at {}:{}",
        config.caspar_host,
        config.caspar_port
    );

    let tcp_timeout = Duration::from_secs(config.tcp_timeout_secs);
    let amcp = AmcpClient::connect(&config.caspar_host, config.caspar_port, tcp_timeout).await?;

    let mut controller = MediaController::new(&config, amcp).await?;

    let _ = ui_tx.try_send(UiMessage::Layers(state::layer_displays(
        controller.layer_states(),
    )));

    let device = get_audio_device(&config)?;
    let device_config = device.default_input_config()?;
    let sample_rate = device_config.sample_rate();
    let channels: usize = device_config.channels().into();
    anyhow::ensure!(channels >= 1, "audio device reports 0 channels");
    anyhow::ensure!(
        config.audio_channel < channels,
        "audio_channel {} out of range for device with {} channels",
        config.audio_channel,
        channels,
    );

    tracing::info!(
        "audio device: {} @ {} Hz ({} ch, reading ch {})",
        device_name(&device),
        sample_rate,
        channels,
        config.audio_channel,
    );
    tracing::info!("listening for LTC timecode (Ctrl+C to stop)");

    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<f32>>(64);
    let stream = build_audio_stream(&device, &device_config, channels, config.audio_channel, tx)?;
    stream.play()?;

    let mut parser = TimecodeParser::new(
        sample_rate,
        config.pause_detection_threshold_ms,
        config.tc_fallback_fps,
    );

    let pause_check_interval = Duration::from_millis(config.pause_detection_threshold_ms / 2);

    loop {
        // Wait for audio samples, check pause timeout, or handle shutdown.
        let received = tokio::select! {
            samples = rx.recv() => match samples {
                Some(s) => Some(s),
                None => break, // audio channel closed
            },
            _ = tokio::time::sleep(pause_check_interval) => None,
            _ = token.cancelled() => {
                tracing::info!("shutting down");
                break;
            }
        };

        let now = Instant::now();

        if let Some(samples) = received {
            parser.push(&samples, now);
        }

        while let Some(event) = parser.next(now) {
            let (tc, status) = match &event {
                TimecodeEvent::Playing(pos) => (pos.to_string(), TcStatus::Playing),
                TimecodeEvent::Paused(pos) => (pos.to_string(), TcStatus::Paused),
            };
            let _ = ui_tx.try_send(UiMessage::Timecode { tc, status });

            if let Err(e) = controller.handle_event(&event).await {
                tracing::error!("AMCP error: {}", e);
            }

            let _ = ui_tx.try_send(UiMessage::Layers(state::layer_displays(
                controller.layer_states(),
            )));
        }
    }

    Ok(())
}

/// Build the cpal input stream, converting samples to mono f32.
fn build_audio_stream(
    device: &cpal::Device,
    device_config: &cpal::SupportedStreamConfig,
    channels: usize,
    channel: usize,
    tx: tokio::sync::mpsc::Sender<Vec<f32>>,
) -> Result<cpal::Stream> {
    let err_fn = |err: cpal::StreamError| {
        tracing::error!("audio stream error: {}", err);
    };

    let tx_i16 = tx.clone();

    let stream = match device_config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &device_config.config(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let samples: Vec<f32> = data.iter().skip(channel).step_by(channels).copied().collect();
                let _ = tx.try_send(samples);
            },
            err_fn,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_input_stream(
            &device_config.config(),
            move |data: &[i16], _: &cpal::InputCallbackInfo| {
                let samples: Vec<f32> = data
                    .iter()
                    .skip(channel)
                    .step_by(channels)
                    .map(|&s| s as f32 / i16::MAX as f32)
                    .collect();
                let _ = tx_i16.try_send(samples);
            },
            err_fn,
            None,
        )?,
        fmt => return Err(anyhow!("unsupported sample format: {:?}", fmt)),
    };

    Ok(stream)
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    if args.list_devices {
        return list_audio_devices();
    }

    let config = Config::from_file(&args.config)?;

    let (ui_tx, ui_rx) = mpsc::channel::<UiMessage>(256);

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with(tui::log_layer::TuiLogLayer::new(ui_tx.clone()))
        .init();

    tracing::info!("loaded config from: {}", args.config);

    // Run the main application logic, concurrently with the TUI.
    let result = {
        let _tui = tui::enter()?;

        let token = CancellationToken::new();

        // Cancel on SIGINT/SIGTERM.
        let signal_token = token.clone();
        tokio::spawn(async move {
            #[cfg(unix)]
            {
                use tokio::signal::unix::{SignalKind, signal};
                let mut sigterm =
                    signal(SignalKind::terminate()).expect("failed to register SIGTERM handler");
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = sigterm.recv() => {}
                }
            }
            #[cfg(not(unix))]
            tokio::signal::ctrl_c().await.ok();

            signal_token.cancel();
        });

        let tui_task = tokio::spawn(tui::run(ui_rx, token.clone()));

        let r = run(config, ui_tx, token.clone()).await;

        token.cancel();
        let _ = tui_task.await;

        r
    };

    result
}
