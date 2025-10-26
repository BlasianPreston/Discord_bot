use anyhow::Result;
use dotenv::dotenv;
use futures_util::{SinkExt, stream::StreamExt};
use serde_json::Value;
use std::collections::HashMap;
use std::{env, time::Duration};
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};

pub async fn gateway_connect() -> Result<()> {
    let url = "wss://gateway.discord.gg/?v=10&encoding=json";
    let (ws_stream, _) = connect_async(url).await?;
    println!("Connected to Discord gateway!");

    let (mut write, mut read) = ws_stream.split();

    // Send info through tx_outbound and receive it through rx_outbound which sends it to write which sends it to discord
    let (tx_outbound, mut rx_outbound) = mpsc::channel::<String>(32);

    // Anything we want to handle ourselves and then send back to discord, send through tx_inbound which is received by rx_inbound
    let (tx_inbound, mut rx_inbound) = mpsc::channel::<Value>(32);

    // Send s value to heartbeat
    let (seq_tx, seq_rx) = tokio::sync::watch::channel(None::<u64>);

    let mut users_to_channels = HashMap::new();

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
                            "intents": 33281, // GUILDS + GUILD_MESSAGES + MESSAGE_CONTENT
                            "properties": { "$os": "linux", "$browser": "rust-bot", "$device": "rust-bot" }
                        }
                    });

                    // Send Identify
                    tx_outbound_clone.send(identify.to_string()).await.unwrap();
                    println!("Sent Identify payload.");

                    // Spawn the heartbeat task after receiving the interval
                    let tx_heartbeat = tx_outbound_clone.clone();
                    let seq_val = *seq_rx.borrow();
                    tokio::spawn(async move {
                        loop {
                            tokio::time::sleep(Duration::from_millis(interval_ms)).await;
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
                // Opcode 11: Heartbeat ACK
                Some(11) => {
                    println!("Received Heartbeat ACK.");
                }
                // Opcode 1: Immediate heartbeat
                Some(1) => {
                    let seq_val = *seq_rx.borrow();
                    let heartbeat = serde_json::json!({"op": 1, "d": seq_val}).to_string();
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

            let d_content = &json["d"];
            match json["op"].as_u64() {
                Some(0) => {
                    let event_name = json["t"].as_str();
                    match event_name {
                        Some("READY") => {
                            if let Some(s) = json["s"].as_u64() {
                                let _ = seq_tx.send(Some(s));
                            }
                        }

                        Some("VOICE_STATE_UPDATE") => {
                            let channel_id = d_content["channel_id"].as_u64();
                            let user_id = d_content["user_id"].as_u64().unwrap_or(0);
                            match channel_id {
                                None => {
                                    users_to_channels.remove(&user_id);
                                }

                                Some(id) => {
                                    users_to_channels.insert(user_id, id);
                                }
                            }
                        }

                        Some("MESSAGE_CREATE") => {
                            let bot_id =
                                env::var("DISCORD_APP_ID").expect("DISCORD_APP_ID not set").parse().unwrap_or(0);
                            let content = d_content["content"].as_str().unwrap_or("");
                            let author_id = d_content["id"].as_u64().unwrap_or(0);
                            let channel_id = d_content["channel_id"].as_str().unwrap_or("");
                            let guild_id = users_to_channels[&author_id].to_string();

                            // Ignore your own messages
                            if author_id == (bot_id) {
                                return;
                            }

                            if content.starts_with("!askleo join") {
                                let join_json = serde_json::json!({
                                  "op": 4,
                                  "d": {
                                    "guild_id": guild_id,
                                    "channel_id": channel_id,
                                    "self_mute": false,
                                    "self_deaf": false
                                  }
                                });
                                if tx_outbound
                                    .send(Message::Text(join_json.to_string().into()).to_string())
                                    .await
                                    .is_err()
                                {
                                    println!("Error sending message");
                                }
                                println!("Successfully joined voice channel");
                            }

                            if content.starts_with("!askleo randomleo") {
                                println!("Random Picture of Leo asked for") // Implement this later
                            }

                            if content.starts_with("!askleo leave") {
                                // Implement this later
                            }
                        }

                        Some(_) => {
                            println!("Unhandled event specifier");
                        }

                        None => {
                            println!("No event specifier found");
                        }
                    }
                }
                Some(_) => {
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
