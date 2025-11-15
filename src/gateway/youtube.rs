use anyhow::Result;
use audiopus::coder::Encoder;
use audiopus::{Application, Channels, SampleRate};
use std::process::Stdio;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command as TokioCommand;
use tokio::sync::mpsc;
use xsalsa20poly1305::{Key, KeyInit, Nonce, XSalsa20Poly1305, aead::Aead};

pub async fn get_youtube_audio_packets(
    youtube_url: &str,
    secret_key: &[u8; 32],
    ssrc: u32,
) -> Result<mpsc::Receiver<Vec<u8>>> {
    let (tx, rx) = mpsc::channel(100);

    // Spawn yt-dlp to get audio stream
    let mut ytdlp = TokioCommand::new("yt-dlp")
        .args(["-f", "bestaudio", "-o", "-"])
        .arg(youtube_url)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;

    let ytdlp_stdout = ytdlp
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("Failed to get yt-dlp stdout"))?;

    // Spawn ffmpeg to convert to PCM
    let mut ffmpeg = TokioCommand::new("ffmpeg")
        .args([
            "-i",
            "pipe:0",
            "-f",
            "s16le",
            "-ar",
            "48000",
            "-ac",
            "2",
            "-loglevel",
            "error",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;

    let ffmpeg_stdin = ffmpeg
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("Failed to get ffmpeg stdin"))?;
    let mut ffmpeg_stdout = ffmpeg
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("Failed to get ffmpeg stdout"))?;

    // Create Opus encoder
    let opus_encoder = Encoder::new(SampleRate::Hz48000, Channels::Stereo, Application::Audio)?;

    // Create cipher for encryption
    let key = Key::from_slice(secret_key);
    let cipher = XSalsa20Poly1305::new(key);

    // Spawn task to pipe yt-dlp output to ffmpeg input
    let mut ytdlp_stdout = tokio::io::BufReader::new(ytdlp_stdout);
    let mut ffmpeg_stdin = ffmpeg_stdin;
    tokio::spawn(async move {
        let mut buffer = vec![0u8; 8192];
        loop {
            match ytdlp_stdout.read(&mut buffer).await {
                Ok(0) => break,
                Ok(n) => {
                    if ffmpeg_stdin.write_all(&buffer[..n]).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = ffmpeg_stdin.shutdown().await;
    });

    // Spawn task to process audio and generate packets
    let tx_clone = tx.clone();
    tokio::spawn(async move {
        let mut seq: u16 = 0;
        let mut timestamp: u32 = 0;
        let mut opus_buf = vec![0u8; 4000];
        let pcm_frame = vec![0i16; 1920]; // 20ms of stereo audio at 48kHz
        let mut pcm_frame_mut = pcm_frame;

        loop {
            // Read PCM data (1920 samples = 20ms of stereo audio)
            let bytes_to_read = pcm_frame_mut.len() * 2; // 2 bytes per i16 sample
            let mut pcm_bytes = vec![0u8; bytes_to_read];

            match ffmpeg_stdout.read_exact(&mut pcm_bytes).await {
                Ok(_) => {
                    // Convert bytes to i16 samples
                    for (i, chunk) in pcm_bytes.chunks_exact(2).enumerate() {
                        pcm_frame_mut[i] = i16::from_le_bytes([chunk[0], chunk[1]]);
                    }
                }
                Err(_) => {
                    // EOF or error, break
                    break;
                }
            }

            // Encode to Opus
            match opus_encoder.encode(&pcm_frame_mut, &mut opus_buf) {
                Ok(encoded_size) if encoded_size > 0 => {
                    let opus_packet = &opus_buf[..encoded_size];

                    // Create RTP header
                    let rtp_header = make_rtp_header(seq, timestamp, ssrc);

                    // Encrypt the packet
                    let encrypted_packet =
                        match construct_encrypted_packet(&cipher, opus_packet, rtp_header) {
                            Some(packet) => packet,
                            None => continue,
                        };

                    // Send packet through channel
                    if tx_clone.send(encrypted_packet).await.is_err() {
                        break;
                    }

                    // Update sequence and timestamp
                    seq = seq.wrapping_add(1);
                    timestamp = timestamp.wrapping_add(960); // 960 samples = 20ms at 48kHz
                }
                Ok(_) => continue,
                Err(_) => continue,
            }
        }
    });

    Ok(rx)
}

fn make_rtp_header(seq: u16, timestamp: u32, ssrc: u32) -> [u8; 12] {
    let mut header = [0u8; 12];

    header[0] = 0x80; // Version = 2
    header[1] = 0x78; // Payload type = 120

    header[2] = (seq >> 8) as u8;
    header[3] = seq as u8;

    header[4] = (timestamp >> 24) as u8;
    header[5] = (timestamp >> 16) as u8;
    header[6] = (timestamp >> 8) as u8;
    header[7] = timestamp as u8;

    header[8] = (ssrc >> 24) as u8;
    header[9] = (ssrc >> 16) as u8;
    header[10] = (ssrc >> 8) as u8;
    header[11] = ssrc as u8;

    header
}

fn construct_encrypted_packet(
    cipher: &XSalsa20Poly1305,
    opus_packet: &[u8],
    rtp_header: [u8; 12],
) -> Option<Vec<u8>> {
    let mut nonce = [0u8; 24];
    nonce[..12].copy_from_slice(&rtp_header);

    let nonce_obj = Nonce::from_slice(&nonce);
    let encrypted_payload = cipher.encrypt(&nonce_obj, opus_packet).ok()?;

    let mut packet = Vec::with_capacity(12 + encrypted_payload.len());
    packet.extend_from_slice(&rtp_header);
    packet.extend_from_slice(&encrypted_payload);

    Some(packet)
}
