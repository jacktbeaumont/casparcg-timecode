//! AMCP client.

use anyhow::{Result, anyhow};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::TcpStream;
use tokio::time::timeout;

/// Media type returned by CINF command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaType {
    Movie,
    Still,
    Audio,
}

/// Media file information from CasparCG.
#[derive(Debug, Clone)]
pub struct MediaInfo {
    #[allow(dead_code)]
    pub filename: String,
    pub media_type: MediaType,
    pub frame_count: u32,
    pub frame_rate: f32,
}

/// AMCP client for communicating with CasparCG Server.
///
/// If the TCP connection drops, commands will automatically reconnect
/// and retry once before returning an error.
pub struct AmcpClient {
    host: String,
    port: u16,
    tcp_timeout: Duration,
    writer: BufWriter<tokio::io::WriteHalf<TcpStream>>,
    reader: BufReader<tokio::io::ReadHalf<TcpStream>>,
}

impl AmcpClient {
    /// Connect to a CasparCG server.
    #[tracing::instrument(skip(tcp_timeout), fields(addr = %format!("{}:{}", host, port)))]
    pub async fn connect(host: &str, port: u16, tcp_timeout: Duration) -> Result<Self> {
        let (reader, writer) = Self::tcp_connect(host, port, tcp_timeout).await?;
        Ok(Self {
            host: host.to_string(),
            port,
            tcp_timeout,
            reader,
            writer,
        })
    }

    /// Open a TCP connection and return the reader/writer pair.
    async fn tcp_connect(
        host: &str,
        port: u16,
        tcp_timeout: Duration,
    ) -> Result<(
        BufReader<tokio::io::ReadHalf<TcpStream>>,
        BufWriter<tokio::io::WriteHalf<TcpStream>>,
    )> {
        let addr = format!("{}:{}", host, port);
        let stream = timeout(tcp_timeout, TcpStream::connect(&addr))
            .await
            .map_err(|_| anyhow!("connection to {} timed out", addr))?
            .map_err(|e| anyhow!("failed to connect to {}: {}", addr, e))?;

        let (r, w) = tokio::io::split(stream);
        Ok((BufReader::new(r), BufWriter::new(w)))
    }

    /// Replace the current connection with a fresh one.
    async fn reconnect(&mut self) -> Result<()> {
        let (reader, writer) = Self::tcp_connect(&self.host, self.port, self.tcp_timeout).await?;
        self.reader = reader;
        self.writer = writer;
        tracing::info!("Reconnected to CasparCG at {}:{}", self.host, self.port);
        Ok(())
    }

    /// Send a command and wait for response.
    /// On I/O failure, reconnects once and retries before returning an error.
    #[tracing::instrument(skip(self))]
    async fn send(&mut self, command: &str) -> Result<String> {
        match self.send_once(command).await {
            Ok(response) => Ok(response),
            Err(first_err) => {
                tracing::warn!("AMCP command failed ({}), reconnecting", first_err);
                self.reconnect().await?;
                self.send_once(command).await
            }
        }
    }

    /// Send a command on the current connection (no retry).
    async fn send_once(&mut self, command: &str) -> Result<String> {
        let t = self.tcp_timeout;

        // Send command with CRLF terminator
        timeout(t, async {
            self.writer
                .write_all(format!("{}\r\n", command).as_bytes())
                .await?;
            self.writer.flush().await?;
            Ok::<_, anyhow::Error>(())
        })
        .await
        .map_err(|_| anyhow!("send timed out: {}", command))??;

        // Read response status line
        let mut response = String::new();
        timeout(t, self.reader.read_line(&mut response))
            .await
            .map_err(|_| anyhow!("response timed out: {}", command))??;

        if response.starts_with('2') {
            Ok(response)
        } else {
            Err(anyhow!("AMCP error: {}", response.trim()))
        }
    }

    /// Read a single line from the server with timeout.
    async fn read_line(&mut self) -> Result<String> {
        let mut line = String::new();
        timeout(self.tcp_timeout, self.reader.read_line(&mut line))
            .await
            .map_err(|_| anyhow!("read timed out"))??;
        Ok(line)
    }

    /// Begin a batch transaction.
    ///
    /// All subsequent commands are queued until [`end_batch`](Self::end_batch)
    /// is called, at which point they execute atomically on the server.
    pub async fn begin_batch(&mut self) -> Result<()> {
        self.send("BEGIN").await?;
        Ok(())
    }

    /// Commit a batch transaction, executing all queued commands atomically.
    pub async fn end_batch(&mut self) -> Result<()> {
        self.send("COMMIT").await?;
        Ok(())
    }

    /// Discard a batch transaction without executing.
    pub async fn discard_batch(&mut self) -> Result<()> {
        self.send("DISCARD").await?;
        Ok(())
    }

    /// Play a file on a layer, optionally seeking to a frame
    ///
    /// # Arguments
    ///
    /// * `channel` - CasparCG channel number
    /// * `layer` - Layer number
    /// * `file` - File path to play
    /// * `seek_frame` - Optional frame to seek to on play
    #[tracing::instrument(skip(self), level = "debug")]
    pub async fn play(
        &mut self,
        channel: u16,
        layer: u16,
        file: &str,
        seek_frame: Option<u32>,
    ) -> Result<()> {
        let seek = seek_frame.map(|f| format!(" SEEK {f}")).unwrap_or_default();
        let cmd = format!("PLAY {channel}-{layer} \"{file}\"{seek}");
        self.send(&cmd).await?;
        Ok(())
    }

    /// Pause playback on a layer
    ///
    /// # Arguments
    ///
    /// * `channel` - CasparCG channel number
    /// * `layer` - Layer number
    #[tracing::instrument(skip(self), level = "debug")]
    pub async fn pause(&mut self, channel: u16, layer: u16) -> Result<()> {
        self.send(&format!("PAUSE {}-{}", channel, layer)).await?;
        Ok(())
    }

    /// Stop and clear a layer
    ///
    /// # Arguments
    ///
    /// * `channel` - CasparCG channel number
    /// * `layer` - Layer number
    #[tracing::instrument(skip(self), level = "debug")]
    pub async fn stop(&mut self, channel: u16, layer: u16) -> Result<()> {
        self.send(&format!("STOP {}-{}", channel, layer)).await?;
        Ok(())
    }

    /// Get the output frame rate of a CasparCG channel.
    ///
    /// Sends `INFO [channel]` and parses the `<framerate>` elements from the XML response.
    #[tracing::instrument(skip(self), level = "debug")]
    pub async fn channel_fps(&mut self, channel: u16) -> Result<f32> {
        self.send(&format!("INFO {}", channel)).await?;

        // Read XML lines until </channel> or empty line
        let mut xml = String::new();
        loop {
            let line = self.read_line().await?;
            let done = line.trim().is_empty() || line.contains("</channel>");
            xml.push_str(&line);
            if done {
                break;
            }
        }

        Self::parse_channel_fps(&xml)
    }

    /// Parse channel fps from `INFO [channel]` XML response.
    ///
    /// Expects two `<framerate>` elements representing numerator and denominator.
    fn parse_channel_fps(xml: &str) -> Result<f32> {
        let values: Vec<f32> = xml
            .split("<framerate>")
            .skip(1)
            .take(2)
            .filter_map(|s| s.split("</framerate>").next()?.trim().parse().ok())
            .collect();

        match values.as_slice() {
            [num, den] if *den != 0.0 => Ok(num / den),
            _ => Err(anyhow!(
                "could not parse channel framerate from INFO response"
            )),
        }
    }

    /// Get information about a media file
    ///
    /// # Arguments
    ///
    /// * `filename` - File path to query
    ///
    /// Returns MediaInfo with file details including frame count.
    #[tracing::instrument(skip(self), level = "debug")]
    pub async fn cinf(&mut self, filename: &str) -> Result<MediaInfo> {
        let response = self.send(&format!("CINF \"{filename}\"")).await?;

        // 200 responses return one or more data lines terminated by an empty line.
        // 201 responses return exactly one data line.
        let first_line = self.read_line().await?;

        if response.starts_with("200") {
            let mut extra_count = 0u32;
            loop {
                let line = self.read_line().await?;
                if line.trim().is_empty() {
                    break;
                }
                extra_count += 1;
            }
            if extra_count > 0 {
                tracing::warn!(
                    "cinf returned {} additional match(es) for '{}'; using first result only. \
                     specify the full path to avoid ambiguity",
                    extra_count,
                    filename
                );
            }
        }

        Self::parse_cinf_response(&first_line, filename)
    }

    /// Parse CINF response line into MediaInfo.
    fn parse_cinf_response(line: &str, filename: &str) -> Result<MediaInfo> {
        // Format: "FILENAME" TYPE FILESIZE LASTMODIFIED FRAMECOUNT FRAMERATE
        // Example: "AMB" MOVIE 6445960 20170413102935 268 1/25

        let line = line.trim();

        // Filename is discarded to use request filename
        let parts: Vec<&str> = if line.starts_with('"') {
            let after_quote = line[1..]
                .find('"')
                .ok_or_else(|| anyhow!("failed to parse CINF response"))?;
            line.get(after_quote + 3..)
                .ok_or_else(|| anyhow!("failed to parse CINF response: {}", line))?
                .split_whitespace()
                .collect()
        } else {
            line.split_whitespace().skip(1).collect()
        };

        // parts: [type, filesize, lastmodified, framecount, framerate]
        if parts.len() < 5 {
            return Err(anyhow!("failed to parse CINF response: {}", line));
        }

        let media_type = match parts[0].to_uppercase().as_str() {
            "MOVIE" => MediaType::Movie,
            "STILL" => MediaType::Still,
            "AUDIO" => MediaType::Audio,
            t => return Err(anyhow!("unknown media type: {}", t)),
        };

        let frame_count: u32 = parts[3]
            .parse()
            .map_err(|_| anyhow!("invalid frame count in CINF response: {}", parts[3]))?;

        let frame_rate = match parts[4].split_once('/') {
            Some((num, den)) => {
                let num: f32 = num.parse().map_err(|_| {
                    anyhow!(
                        "invalid frame rate numerator in CINF response: {}",
                        parts[4]
                    )
                })?;
                let den: f32 = den.parse().map_err(|_| {
                    anyhow!(
                        "invalid frame rate denominator in CINF response: {}",
                        parts[4]
                    )
                })?;
                den / num
            }
            None => parts[4]
                .parse()
                .map_err(|_| anyhow!("invalid frame rate in CINF response: {}", parts[4]))?,
        };

        Ok(MediaInfo {
            filename: filename.to_string(),
            media_type,
            frame_count,
            frame_rate,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_channel_fps_50() {
        let xml = "<channel><format>720p5000</format><framerate>50</framerate><framerate>1</framerate></channel>";
        let fps = AmcpClient::parse_channel_fps(xml).unwrap();
        assert_eq!(fps, 50.0);
    }

    #[test]
    fn test_parse_channel_fps_25() {
        let xml = "<channel><format>1080i5000</format><framerate>25</framerate><framerate>1</framerate></channel>";
        let fps = AmcpClient::parse_channel_fps(xml).unwrap();
        assert_eq!(fps, 25.0);
    }

    #[test]
    fn test_cinf_parses_spec_example() {
        let info =
            AmcpClient::parse_cinf_response("\"AMB\" MOVIE 6445960 20170413102935 268 1/25", "AMB")
                .unwrap();
        assert_eq!(info.filename, "AMB");
        assert_eq!(info.media_type, MediaType::Movie);
        assert_eq!(info.frame_count, 268);
        assert_eq!(info.frame_rate, 25.0);
    }

    #[test]
    fn test_cinf_parses_still_media_type() {
        let info = AmcpClient::parse_cinf_response(
            "\"IMAGE\" STILL 1024000 20200101T120000 1 1/1",
            "IMAGE",
        )
        .unwrap();
        assert_eq!(info.media_type, MediaType::Still);
        assert_eq!(info.frame_count, 1);
        assert_eq!(info.frame_rate, 1.0);
    }

    #[test]
    fn test_cinf_parses_audio_media_type() {
        let info = AmcpClient::parse_cinf_response(
            "\"SOUND\" AUDIO 2048000 20210615T093000 1000 1/48000",
            "SOUND",
        )
        .unwrap();
        assert_eq!(info.media_type, MediaType::Audio);
        assert_eq!(info.frame_count, 1000);
        assert_eq!(info.frame_rate, 48000.0);
    }

    #[test]
    fn test_cinf_parses_spaced_filename() {
        let info = AmcpClient::parse_cinf_response(
            "\"MY VIDEO FILE\" MOVIE 5000000 20240101T120000 500 1/25",
            "MY VIDEO FILE",
        )
        .unwrap();
        assert_eq!(info.filename, "MY VIDEO FILE");
        assert_eq!(info.frame_count, 500);
        assert_eq!(info.frame_rate, 25.0);
    }

    #[test]
    fn test_cinf_errors_on_unknown_media_type() {
        let err = AmcpClient::parse_cinf_response(
            "\"FILE\" UNKNOWN 1000 20200101T120000 100 1/25",
            "FILE",
        )
        .unwrap_err();
        assert!(err.to_string().contains("unknown media type"));
    }

    #[test]
    fn test_cinf_errors_on_invalid_frame_count() {
        let err = AmcpClient::parse_cinf_response(
            "\"FILE\" MOVIE 1000 20200101T120000 INVALID 1/25",
            "FILE",
        )
        .unwrap_err();
        assert!(err.to_string().contains("invalid frame count"));
    }

    #[test]
    fn test_cinf_errors_on_invalid_frame_rate() {
        let err = AmcpClient::parse_cinf_response(
            "\"FILE\" MOVIE 1000 20200101T120000 100 INVALID",
            "FILE",
        )
        .unwrap_err();
        assert!(err.to_string().contains("invalid frame rate"));
    }

    #[test]
    fn test_cinf_errors_on_missing_fields() {
        let err = AmcpClient::parse_cinf_response("\"FILE\" MOVIE", "FILE").unwrap_err();
        assert!(err.to_string().contains("failed to parse CINF response"));
    }
}
