use anyhow::Result;
use futures_util::{SinkExt, stream::StreamExt};
use serde_json::{Value, json};
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::net::UdpSocket;
use tokio::select;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use url::Url;

use crate::gateway::youtube;

fn current_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
}

// Create join voice gateway function
pub async fn connect_to_voice_server(
    guild_id: String,
    endpoint: String,
    token: String,
    session_id: String,
    user_id: String,
    youtube_url: String,
) -> Result<()> {
    // Step 1: connect to wss://{endpoint}?v=4
    // Step 2: send Identify payload
    // Step 3: handle Hello + Ready
    // Step 4: UDP discovery
    // Step 5: send Select Protocol
    // Step 6: wait for Session Description
    // Step 7: return VoiceConnection with key + sockets

    // Validate endpoint
    if endpoint.is_empty() {
        anyhow::bail!("Voice server endpoint is empty");
    }

    // Log the raw endpoint for debugging
    println!("Raw endpoint received: {}", endpoint);

    // Remove port from endpoint - WebSocket always uses default port 443
    // The port in the endpoint is only for UDP connections
    let endpoint_host = if let Some(colon_pos) = endpoint.find(':') {
        &endpoint[..colon_pos]
    } else {
        &endpoint
    };

    println!("Endpoint host (after port removal): {}", endpoint_host);

    // Discord voice WebSocket format: wss://{endpoint}?v=4 (no port, uses default 443)
    // Use url crate to properly construct the URL
    let voice_url = Url::parse(&format!("wss://{}", endpoint_host))
        .and_then(|mut url| {
            url.set_query(Some("v=4"));
            Ok(url.to_string())
        })
        .unwrap_or_else(|_| format!("wss://{}?v=4", endpoint_host));

    println!("Connecting to voice WebSocket: {}", voice_url);
    println!(
        "Using token: {}...",
        if token.len() > 10 {
            &token[..10]
        } else {
            &token
        }
    );
    println!("Session ID: {}", session_id);
    println!("User ID: {}", user_id);
    println!("Guild ID: {}", guild_id);

    // Longer delay to ensure voice server is fully ready
    // Discord needs time to process the voice state and server updates
    println!("Waiting 2 seconds for voice server to be ready...");
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Try connecting without port first (standard approach)
    let (ws_stream, _response) = match connect_async(&voice_url).await {
        Ok((stream, resp)) => {
            println!("Connected to voice WebSocket!");
            println!("Response status: {:?}", resp.status());
            (stream, resp)
        }
        Err(e) => {
            eprintln!("WebSocket connection failed without port: {:?}", e);
            // Try with port as fallback (some Discord endpoints may require it)
            if endpoint.contains(':') {
                let endpoint_with_port = format!("wss://{}?v=4", endpoint);
                println!(
                    "Trying fallback connection with port: {}",
                    endpoint_with_port
                );
                match connect_async(&endpoint_with_port).await {
                    Ok((stream, resp)) => {
                        println!("Connected to voice WebSocket with port!");
                        println!("Response status: {:?}", resp.status());
                        (stream, resp)
                    }
                    Err(e2) => {
                        eprintln!("WebSocket connection also failed with port: {:?}", e2);
                        anyhow::bail!(
                            "Failed to connect to voice WebSocket (tried both with and without port): {}",
                            e
                        );
                    }
                }
            } else {
                anyhow::bail!("Failed to connect to voice WebSocket {}: {}", voice_url, e);
            }
        }
    };

    let (mut write, mut read) = ws_stream.split();

    let (tx_outbound, mut rx_outbound) = mpsc::channel::<String>(32);
    let (speaking_tx, speaking_rx) = tokio::sync::watch::channel(None::<u64>);
    let (address_tx, address_rx) = tokio::sync::watch::channel(None::<String>);
    let (port_tx, port_rx) = tokio::sync::watch::channel(None::<u16>);
    let youtube_url_clone = youtube_url.clone();

    let tx_outbound_clone = tx_outbound.clone();
    tokio::spawn(async move {
        loop {
            select! {
                msg = rx_outbound.recv() => {
                    if let Some(msg) = msg {
                        let json: Value = serde_json::from_str(&msg).unwrap_or_default();
                        match json["op"].as_u64() { // We need to send out op codes 0 and 1
                            Some(_) => {
                                if write.send(Message::Text(msg.into())).await.is_err() {
                                    eprintln!("Error sending message or WebSocket closed");
                                    break;
                                }
                            }
                            None => {println!("Invalid Json sent to tx_outbound")}
                    }
                    }
                }
            }
        }
    });

    tokio::spawn(async move {
        loop {
            select! {
                msg = read.next() => {
                    if let Some(msg) = msg {
                        let msg = match msg {
                            Ok(Message::Text(text)) => text,
                            Ok(_) => {
                                continue;
                            }
                            Err(e) => {
                                eprintln!("Error reading from voice WebSocket: {e}");
                                break;
                            }
                        };

                        let json: Value = match serde_json::from_str(&msg) {
                            Ok(val) => val,
                            Err(e) => {
                                eprintln!("Error parsing JSON: {e}");
                                continue;
                            }
                        };

                        match json["op"].as_u64() {
                            Some(2) => { // Store port and address
                                println!("Received Ready (opcode 2) from voice server");
                                let data = &json["d"]["data"];

                                // Extract values before spawning task to avoid lifetime issues
                                let address_str = data["address"].as_str().unwrap_or("").to_string();
                                let port_num = data["port"].as_u64().unwrap_or(0) as u16;

                                // Store those two values alongside ssrc
                                // Use ssrc when sending voice packets

                                if let Some(s) = data["ssrc"].as_u64() {
                                    println!("Got SSRC: {}", s);
                                    let _ = speaking_tx.send(Some(s));
                                }

                                if let Some(addr) = data["address"].as_str() {
                                    println!("Got address: {}", addr);
                                    let _ = address_tx.send(Some(addr.to_string()));
                                }

                                if let Some(p) = data["port"].as_u64() {
                                    println!("Got port: {}", p);
                                    let _ = port_tx.send(Some(p as u16));
                                }

                                // Do UDP IP discovery
                                let tx_outbound_udp = tx_outbound.clone();
                                let mut speaking_rx_udp = speaking_rx.clone();
                                let address_str_clone = address_str.clone();
                                tokio::spawn(async move {
                                    // Wait for SSRC
                                    if speaking_rx_udp.borrow().is_none() {
                                        speaking_rx_udp.changed().await.ok();
                                    }

                                    let ssrc = match *speaking_rx_udp.borrow() {
                                        Some(s) => s,
                                        None => {
                                            eprintln!("SSRC not available for UDP discovery");
                                            return;
                                        }
                                    };

                                    // Create UDP socket for discovery
                                    let socket = match UdpSocket::bind("0.0.0.0:0").await {
                                        Ok(s) => s,
                                        Err(e) => {
                                            eprintln!("Failed to create UDP socket for discovery: {}", e);
                                            return;
                                        }
                                    };

                                    // Send discovery packet (70 bytes: 4 bytes SSRC + 66 bytes padding)
                                    let mut discovery_packet = vec![0u8; 70];
                                    discovery_packet[0..4].copy_from_slice(&ssrc.to_be_bytes());

                                    let discovery_addr = format!("{}:{}", address_str_clone, port_num);
                                    println!("Sending UDP discovery packet to {}", discovery_addr);

                                    if let Err(e) = socket.send_to(&discovery_packet, &discovery_addr).await {
                                        eprintln!("Failed to send UDP discovery packet: {}", e);
                                        return;
                                    }

                                    // Receive response (should contain our external IP)
                                    let mut buf = [0u8; 70];
                                    match socket.recv_from(&mut buf).await {
                                        Ok((size, _)) => {
                                            if size >= 4 {
                                                // Response format: [4 bytes IP] [2 bytes port] [rest is padding]
                                                let ip_bytes = &buf[0..4];
                                                let port_bytes = &buf[4..6];
                                                let discovered_ip = format!("{}.{}.{}.{}", ip_bytes[0], ip_bytes[1], ip_bytes[2], ip_bytes[3]);
                                                let discovered_port = u16::from_be_bytes([port_bytes[0], port_bytes[1]]);

                                                println!("UDP discovery successful - IP: {}, Port: {}", discovered_ip, discovered_port);

                                                // Send Select Protocol with discovered IP
                                                let select_protocol = json!({
                                                    "op": 1,
                                                    "d": {
                                                        "protocol": "udp",
                                                        "data": {
                                                            "address": discovered_ip,
                                                            "port": discovered_port,
                                                            "mode": "xsalsa20_poly1305"
                                                        }
                                                    }
                                                });

                                                if let Err(e) = tx_outbound_udp.send(select_protocol.to_string()).await {
                                                    eprintln!("Failed to send Select Protocol: {}", e);
                                                } else {
                                                    println!("Sent Select Protocol with discovered IP");
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            eprintln!("Failed to receive UDP discovery response: {}", e);
                                            // Fallback: use 0.0.0.0 as IP (Discord will use the connection IP)
                                            let select_protocol = json!({
                                                "op": 1,
                                                "d": {
                                                    "protocol": "udp",
                                                    "data": {
                                                        "address": "0.0.0.0",
                                                        "port": 0,
                                                        "mode": "xsalsa20_poly1305"
                                                    }
                                                }
                                            });
                                            let _ = tx_outbound_udp.send(select_protocol.to_string()).await;
                                        }
                                    }
                                });
                            }

                            Some(4) => { // Session Description
                                println!("Received Session Description (opcode 4) from voice server");
                                let data = &json["d"]["data"];
                                let secret_key = &data["secret_key"]; // Use when sending voice packets

                                // Parse secret_key from JSON array to [u8; 32]
                                let secret_key_bytes: [u8; 32] = match secret_key.as_array() {
                                    Some(arr) if arr.len() == 32 => {
                                        let mut key = [0u8; 32];
                                        for (i, val) in arr.iter().enumerate() {
                                            if let Some(b) = val.as_u64() {
                                                key[i] = b as u8;
                                            } else {
                                                eprintln!("Invalid secret_key format");
                                                continue;
                                            }
                                        }
                                        key
                                    }
                                    _ => {
                                        eprintln!("Invalid secret_key format");
                                        continue;
                                    }
                                };

                                let tx_heartbeat = tx_outbound_clone.clone();
                                let mut speaking_rx_heartbeat = speaking_rx.clone();
                                let mut address_rx_clone = address_rx.clone();
                                let mut port_rx_clone = port_rx.clone();
                                let youtube_url_for_packets = youtube_url_clone.clone();
                                tokio::spawn(async move {
                                    println!("Starting audio packet generation task...");
                                    // Wait for ssrc, address, and port to be available
                                    if speaking_rx_heartbeat.borrow().is_none() {
                                        println!("Waiting for SSRC...");
                                        speaking_rx_heartbeat.changed().await.ok();
                                    }
                                    if address_rx_clone.borrow().is_none() {
                                        println!("Waiting for address...");
                                        address_rx_clone.changed().await.ok();
                                    }
                                    if port_rx_clone.borrow().is_none() {
                                        println!("Waiting for port...");
                                        port_rx_clone.changed().await.ok();
                                    }

                                    let ssrc = match *speaking_rx_heartbeat.borrow() {
                                        Some(s) => s as u32,
                                        None => {
                                            eprintln!("SSRC not available");
                                            return;
                                        }
                                    };

                                    let address = match address_rx_clone.borrow().as_ref() {
                                        Some(addr) => addr.clone(),
                                        None => {
                                            eprintln!("Address not available");
                                            return;
                                        }
                                    };

                                    let port = match *port_rx_clone.borrow() {
                                        Some(p) => p,
                                        None => {
                                            eprintln!("Port not available");
                                            return;
                                        }
                                    };

                                    // Create UDP socket
                                    let socket = match UdpSocket::bind("0.0.0.0:0").await {
                                        Ok(s) => s,
                                        Err(e) => {
                                            eprintln!("Failed to create UDP socket: {}", e);
                                            return;
                                        }
                                    };

                                    let socket_addr = format!("{}:{}", address, port);
                                    println!("Sending audio packets to {}", socket_addr);
                                    println!("YouTube URL: {}", youtube_url_for_packets);

                                    let mut packet_receiver = match youtube::get_youtube_audio_packets(
                                        &youtube_url_for_packets,
                                        &secret_key_bytes,
                                        ssrc,
                                    )
                                    .await
                                    {
                                        Ok(rx) => {
                                            println!("Successfully started YouTube audio packet stream");
                                            rx
                                        }
                                        Err(e) => {
                                            eprintln!("Failed to get YouTube audio packets: {}", e);
                                            return;
                                        }
                                    };

                                    // Send speaking indicator BEFORE starting audio
                                    let speaking_indicator = serde_json::json!({
                                        "op": 5,
                                        "d": {
                                            "speaking": 1,
                                            "delay": 0,
                                            "ssrc": ssrc
                                        }
                                    })
                                    .to_string();
                                    if let Err(e) = tx_heartbeat.send(speaking_indicator).await {
                                        eprintln!("Failed to send speaking indicator: {}", e);
                                    } else {
                                        println!("Sent speaking indicator (opcode 5) before starting audio");
                                    }

                                    // Spawn heartbeat task to keep sending speaking indicator
                                    let tx_heartbeat_clone = tx_heartbeat.clone();
                                    let speaking_rx_heartbeat_clone = speaking_rx_heartbeat.clone();
                                    tokio::spawn(async move {
                                        loop {
                                            tokio::time::sleep(Duration::from_millis(5000)).await;
                                            let ssrc_val = *speaking_rx_heartbeat_clone.borrow();
                                            if let Some(s) = ssrc_val {
                                                let heartbeat = serde_json::json!({
                                                    "op": 5,
                                                    "d": {
                                                        "speaking": 1,
                                                        "delay": 0,
                                                        "ssrc": s
                                                    }
                                                })
                                                .to_string();
                                                if tx_heartbeat_clone.send(heartbeat).await.is_err() {
                                                    eprintln!("Heartbeat channel closed.");
                                                    break;
                                                }
                                                println!("Sent speaking heartbeat.");
                                            }
                                        }
                                    });

                                    // Small delay before starting audio to ensure Discord is ready
                                    tokio::time::sleep(Duration::from_millis(100)).await;

                                    // Send audio packets via UDP
                                    let mut packet_count = 0;
                                    while let Some(packet) = packet_receiver.recv().await {
                                        packet_count += 1;
                                        if packet_count % 50 == 0 {
                                            println!("Sent {} audio packets", packet_count);
                                        }
                                        if let Err(e) = socket.send_to(&packet, &socket_addr).await {
                                            eprintln!("Failed to send UDP packet: {}", e);
                                            break;
                                        }
                                    }
                                    println!("Audio packet sending finished. Total packets sent: {}", packet_count);
                                });
                            }
                            Some(6) => {
                                println!("Voice Heartbeat Ack Received");
                            }
                            Some(8) => { // Hello Message
                                let interval_ms = json["d"]["heartbeat_interval"].as_u64().unwrap_or(45000);
                                println!("Received heartbeat interval: {} ms", interval_ms);

                                // Send Identify payload
                                let identify_payload = json!({
                                    "op": 0,
                                    "d": {
                                        "server_id": guild_id,
                                        "user_id": user_id,
                                        "session_id": session_id,
                                        "token": token
                                    }
                                });

                                // Send Identify
                                tx_outbound_clone.send(identify_payload.to_string()).await.unwrap();
                                println!("Sent Identify payload.");

                                // Spawn the heartbeat task after receiving the interval
                                let tx_heartbeat = tx_outbound_clone.clone();
                                tokio::spawn(async move {
                                    loop {
                                        tokio::time::sleep(Duration::from_millis(interval_ms)).await;
                                        let nonce = current_millis();
                                        let heartbeat =
                                            serde_json::json!({ "op": 3, "d": nonce }).to_string();
                                        if tx_heartbeat.send(heartbeat).await.is_err() {
                                            eprintln!("Heartbeat channel closed.");
                                            break;
                                        }
                                        println!("Sent heartbeat.");
                                    }
                                });
                            }
                            Some(op) => {println!("Received unhandled opcode: {}", op);}
                            None => {}
                        }
                    }
                }
            }
        }
    });

    Ok(())
}
