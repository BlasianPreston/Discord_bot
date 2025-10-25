use anyhow::Result;
use dotenv::dotenv;
use futures_util::{SinkExt, stream::StreamExt};
use serde_json::Value;
use std::{env, time::Duration};
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};

async fn gateway() -> Result<()> {
    let url = "wss://gateway.discord.gg/?v=10&encoding=json";
    let (ws_stream, _) = connect_async(url).await?;
    println!("Connected to Discord gateway!");

    let (mut write, mut read) = ws_stream.split();

    // Channel for sending messages *to* Discord
    let (tx_outbound, mut rx_outbound) = mpsc::channel::<String>(32);
    // Channel for receiving *events from* Discord
    let (tx_inbound, mut rx_inbound) = mpsc::channel::<Value>(32);

    // Sender task: Reads from rx_outbound and sends messages to the WebSocket
    tokio::spawn(async move {
        while let Some(msg) = rx_outbound.recv().await {
            if write.send(Message::Text(msg.into())).await.is_err() {
                eprintln!("Error sending message or WebSocket closed");
                break;
            }
        }
        println!("Sender task ended.");
    });

    // Reader Task: Reads from the WebSocket and routes messages
    let tx_outbound_clone = tx_outbound.clone();
    tokio::spawn(async move {
        while let Some(msg) = read.next().await {
            let msg = match msg {
                Ok(Message::Text(text)) => text,
                Ok(_) => {
                    // Ignore non-text messages
                    continue;
                }
                Err(e) => {
                    eprintln!("Error reading from WebSocket: {e}");
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
                // Opcode 10: Hello
                Some(10) => {
                    let interval_ms = json["d"]["heartbeat_interval"].as_u64().unwrap();
                    println!("Received heartbeat interval: {} ms", interval_ms);

                    // Send Identify payload
                    dotenv().ok();
                    let token = env::var("DISCORD_TOKEN").expect("DISCORD_TOKEN not set");
                    let identify = serde_json::json!({
                        "op": 2,
                        "d": {
                            "token": token,
                            "intents": 513, // GUILDS + GUILD_MESSAGES
                            "properties": { "$os": "linux", "$browser": "rust-bot", "$device": "rust-bot" }
                        }
                    });

                    // Send Identify
                    tx_outbound_clone.send(identify.to_string()).await.unwrap();
                    println!("Sent Identify payload.");

                    // Spawn the heartbeat task after receiving the interval
                    let tx_heartbeat = tx_outbound_clone.clone();
                    tokio::spawn(async move {
                        loop {
                            tokio::time::sleep(Duration::from_millis(interval_ms)).await;
                            let heartbeat = serde_json::json!({ "op": 1, "d": null }).to_string();
                            if tx_heartbeat.send(heartbeat).await.is_err() {
                                eprintln!("Heartbeat channel closed.");
                                break;
                            }
                            println!("Sent heartbeat.");
                        }
                    });
                }
                // Opcode 11: Heartbeat ACK
                Some(11) => {
                    println!("Received Heartbeat ACK.");
                }
                // Opcode 1: Immediate heartbeat
                Some(1) => {
                    let heartbeat = serde_json::json!({"op": 1, "d": null}).to_string();
                    let tx_heartbeat = tx_outbound_clone.clone();
                    if tx_heartbeat.send(heartbeat).await.is_err() {
                        eprintln!("Failed to send immediate heartbeat");
                        break;
                    }
                    println!("Send immediate heartbeat")
                }
                // Opcode 0: Dispatch (Gateway Event)
                Some(0) => {
                    println!(
                        "Received Event: {}",
                        json["t"].as_str().unwrap_or("UNKNOWN")
                    );
                    // Send the full event payload to the event handler
                    if tx_inbound.send(json).await.is_err() {
                        eprintln!("Inbound event channel closed.");
                        break;
                    }
                }
                // Need to work on other op codes
                Some(op) => {
                    println!("Received unhandled opcode: {}", op);
                }
                None => {}
            }
        }
        println!("Reader task ended.");
    });

    // This task listens for events from the reader
    tokio::spawn(async move {
        while let Some(event) = rx_inbound.recv().await {
            // TODO: Process different events based on `event["t"]`
            let json: Value = match serde_json::from_str(&event.to_string()) {
                Ok(val) => val,
                Err(e) => {
                    eprintln!("Error parsing JSON: {e}");
                    continue;
                }
            };

            match json["op"].as_u64() {
                Some(0) => {
                    let _resume_gateway_url = json["resume_gateway_url"].to_string();
                    let _session_id = json["session_id"].to_string();
                }
                Some(4) => {
                    // Assume we received a message with channel_id
                    dotenv().ok();
                    let channel_id = "";
                    let join_json = serde_json::json!({
                      "op": 4,
                      "d": {
                        "guild_id": "41771983423143937", // Get rid of hardcoding later
                        "channel_id": channel_id,
                        "self_mute": false,
                        "self_deaf": false
                      }
                    });
                    write.send(Message::Text(join_json.to_string().into())).await; // Might want to get rid of this task and match on the read variable
                }
                Some(op) => {
                    println!("Unhandled opcode")
                }
                None => {
                    println!("Received none")
                }
            }
        }
        println!("Event handler task ended.");
    });

    println!("Gateway connection logic established!");

    // Keep the main function alive
    tokio::signal::ctrl_c().await?;
    println!("Ctrl-C received, shutting down.");

    Ok(())
}
