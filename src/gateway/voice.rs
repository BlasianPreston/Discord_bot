use std::time::Duration;

use anyhow::Result;
use futures_util::{SinkExt, stream::StreamExt};
use serde_json::{Value, json};
use tokio::select;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};

// Create join voice gateway function
async fn connect_to_voice_server(
    guild_id: String,
    endpoint: &str,
    token: String,
    session_id: String,
    user_id: String,
) -> Result<()> {
    // Step 1: connect to wss://{endpoint}?v=4
    // Step 2: send Identify payload
    // Step 3: handle Hello + Ready
    // Step 4: UDP discovery
    // Step 5: send Select Protocol
    // Step 6: wait for Session Description
    // Step 7: return VoiceConnection with key + sockets
    let endpoint = "us-east123.discord.media";
    let voice_url = format!("wss://{}?v=4", endpoint);

    let (mut ws_stream, _) = connect_async(voice_url).await?;
    println!("Connected to voice WebSocket!");

    let (mut write, mut read) = ws_stream.split();

    let (tx_outbound, mut rx_outbound) = mpsc::channel::<String>(32);
    let (seq_tx, seq_rx) = tokio::sync::watch::channel(None::<u64>);

    let identify_payload = json!({
        "op": 0,
        "d": {
            "server_id": guild_id,
            "user_id": user_id,
            "session_id": session_id,
            "token": token
        }
    });

    let tx_outbound_clone = tx_outbound.clone();
    let seq_rx_clone = seq_rx.clone();
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
                                let data = &json["d"]["data"];
                                let address = &data["address"];
                                let port = &data["port"];
                                // Store those two values alongside ssrc
                                // Use ssrc when sending voice packets

                                let select_protocol = json!({
                                    "op": 1,
                                    "d": {
                                        "protocol": "udp",
                                        "data": {
                                            "address": address,
                                            "port": port,
                                            "mode": "xsalsa20_poly1305"
                                        }
                                    }
                                });

                                let _ = tx_outbound.send(select_protocol.to_string());
                            }

                            Some(4) => {
                                let data = &json["d"]["data"];
                                let secret_key = &json["secret_key"];

                                tokio::spawn(async move {
                                    seq_rx_heartbeat.changed().await.unwrap();
                                    loop {
                                        tokio::time::sleep(Duration::from_millis(5000)).await;
                                        let heartbeat =
                                            serde_json::json!({ "op": 1, "d": seq_val }).to_string();
                                        if tx_heartbeat.send(heartbeat).await.is_err() {
                                            eprintln!("Heartbeat channel closed.");
                                            break;
                                        }
                                        println!("Sent heartbeat.");
                                    }
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
                                let mut seq_rx_heartbeat = seq_rx_clone.clone();
                                tokio::spawn(async move {
                                    seq_rx_heartbeat.changed().await.unwrap();
                                    loop {
                                        tokio::time::sleep(Duration::from_millis(interval_ms)).await;
                                        let seq_val = *seq_rx_heartbeat.borrow();
                                        let heartbeat =
                                            serde_json::json!({ "op": 3, "d": seq_val }).to_string();
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
