use super::https::DiscordHTTP;
use anyhow::Result;
use dotenv::dotenv;
use futures_util::{SinkExt, stream::StreamExt};
use serde_json::Value;
use std::collections::HashMap;
use std::{env, time::Duration};
use tokio::select;
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

    // Send values to reconnect
    let (rec_tx, rec_rx) = tokio::sync::watch::channel((None::<String>, None::<String>));

    let (tx_write_update, mut rx_write_update) = mpsc::channel(1);
    let (tx_read_update, mut rx_read_update) = mpsc::channel(1);

    let mut users_to_channels = HashMap::new();

    dotenv().ok();
    let token = env::var("DISCORD_TOKEN").expect("DISCORD_TOKEN not set");

    let seq_rx_clone = seq_rx.clone();
    let rec_rx_clone = rec_rx.clone();
    let token_clone = token.clone();
    let tx_write_update_clone = tx_write_update.clone();
    let tx_read_update_clone = tx_read_update.clone();
    // Sender task: Reads from rx_outbound and sends messages to the WebSocket
    tokio::spawn(async move {
        loop {
            select! {
                msg = rx_outbound.recv() => {
                    if let Some(msg) = msg {
                        let msg_json: Value = serde_json::from_str(&msg).unwrap_or_default();
                        match msg_json["op"].as_u64() {
                            Some(6) => {
                                let (session_id, resume_gateway_url) = rec_rx_clone.borrow().clone();
                                match (session_id, resume_gateway_url) {
                                    (Some(x), Some(y)) => {
                                        let resume_json = serde_json::json!({
                                           "op": 6,
                                           "d": {
                                            "token": token_clone.clone(),
                                            "session_id": x,
                                            "seq": *seq_rx_clone.borrow()
                                           }
                                        });
                                        let (mut new_ws_stream, _) = match connect_async(y).await {
                                            Ok(tuple) => {
                                                println!("Successfully reconnected to resume URL");
                                                tuple
                                            }
                                            Err(e) => {
                                                println!("Error resuming connection: {}", e);
                                                continue;
                                            }
                                        };
                                        let resume_msg = Message::Text(resume_json.to_string().into());
                                        if let Err(e) = new_ws_stream.send(resume_msg).await {
                                            println!("Error sending resume payload: {}", e);
                                            continue; // Skip and try a fresh connect
                                        }

                                        println!("Connection resumed successfully!");

                                        // Split the new stream and send halves to both tasks
                                        let (new_write, new_read) = new_ws_stream.split();
                                        if tx_write_update_clone.send(new_write).await.is_err() {
                                            eprintln!("Failed to send new write stream");
                                            break;
                                        }
                                        if tx_read_update_clone.send(new_read).await.is_err() {
                                            eprintln!("Failed to send new read stream");
                                            break;
                                        }
                                    }
                                    (_, _) => {
                                        println!("No session id or resume gateway url")
                                    }
                                }
                            }
                            Some(_) => {
                                if write.send(Message::Text(msg.into())).await.is_err() {
                                    eprintln!("Error sending message or WebSocket closed");
                                    break;
                                }
                            }
                            None => {
                                println!("Invalid Json sent to tx_outbound");
                            }
                        }
                    } else {
                        // Channel closed
                        break;
                    }
                }
                new_write = rx_write_update.recv() => {
                    if let Some(w) = new_write {
                        write = w;
                        println!("Updated write stream");
                    } else {
                        // Channel closed
                        break;
                    }
                }
            }
        }
        println!("Sender task ended.");
    });

    // Reader Task: Reads from the WebSocket and routes messages
    let tx_outbound_clone = tx_outbound.clone();
    let seq_rx_clone = seq_rx.clone();
    let token_clone_clone = token.clone();
    tokio::spawn(async move {
        loop {
            select! {
                msg = read.next() => {
                    if let Some(msg) = msg {
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
                                let interval_ms = json["d"]["heartbeat_interval"].as_u64().unwrap_or(45000);
                                println!("Received heartbeat interval: {} ms", interval_ms);

                                // Send Identify payload
                                let identify = serde_json::json!({
                                    "op": 2,
                                    "d": {
                                        "token": token_clone_clone.clone(),
                                        "intents": 33281, // GUILDS + GUILD_MESSAGES + MESSAGE_CONTENT
                                        "properties": { "$os": "linux", "$browser": "rust-bot", "$device": "rust-bot" }
                                    }
                                });

                                // Send Identify
                                tx_outbound_clone.send(identify.to_string()).await.unwrap();
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
                                let seq_val = *seq_rx_clone.borrow();
                                let heartbeat = serde_json::json!({"op": 1, "d": seq_val}).to_string();
                                let tx_heartbeat = tx_outbound_clone.clone();
                                if tx_heartbeat.send(heartbeat).await.is_err() {
                                    eprintln!("Failed to send immediate heartbeat");
                                    break;
                                }
                                println!("Send immediate heartbeat");
                            }
                            // Opcode 0: Dispatch (Gateway Event)
                            Some(0) => {
                                println!(
                                    "Received Event: {}",
                                    json["t"].as_str().unwrap_or("UNKNOWN")
                                );
                                if let Some(s) = json["s"].as_u64() {
                                    let _ = seq_tx.send(Some(s));
                                }
                                // Send the full event payload to the event handler
                                if tx_inbound.send(json).await.is_err() {
                                    eprintln!("Inbound event channel closed.");
                                    break;
                                }
                            }
                            Some(7) => {}
                            // Need to work on other op codes
                            Some(op) => {
                                println!("Received unhandled opcode: {}", op);
                            }
                            None => {}
                        }
                    } else {
                        // Stream ended
                        break;
                    }
                }
                new_read = rx_read_update.recv() => {
                    if let Some(r) = new_read {
                        read = r;
                        println!("Updated read stream");
                    } else {
                        // Channel closed
                        break;
                    }
                }
            }
        }
        println!("Reader task ended.");
    });

    // This task listens for events from the reader
    tokio::spawn(async move {
        while let Some(json) = rx_inbound.recv().await {
            // TODO: Process different events based on `event["t"]`

            let d_content = &json["d"];
            match json["op"].as_u64() {
                Some(0) => {
                    let event_name = json["t"].as_str();
                    match event_name {
                        Some("READY") => {
                            let mut session_id = None;
                            let mut resume_gateway_url = None;
                            if let Some(id) = json["d"]["session_id"].as_str() {
                                session_id = Some(id.to_string());
                            }
                            if let Some(url) = json["d"]["resume_gateway_url"].as_str() {
                                resume_gateway_url = Some(url.to_string());
                            }
                            let _ = rec_tx.send((session_id, resume_gateway_url));
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
                            let bot_id = env::var("DISCORD_APP_ID")
                                .expect("DISCORD_APP_ID not set")
                                .parse()
                                .unwrap_or(0);
                            let content = d_content["content"].as_str().unwrap_or("");
                            let author_id = d_content["author"]["id"].as_u64().unwrap_or(0);
                            let channel_id = d_content["channel_id"].as_str().unwrap_or("");
                            let guild_id = d_content["guild_id"].as_str().unwrap_or("");

                            // Ignore your own messages
                            if author_id == (bot_id) {
                                continue;
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
                                if tx_outbound.send(join_json.to_string()).await.is_err() {
                                    println!("Error sending message");
                                }
                                println!("Successfully joined voice channel");
                            }

                            if content.starts_with("!askleo slanderleo") {
                                println!("Random Picture of Leo asked for");
                                let client = DiscordHTTP::new();
                                match DiscordHTTP::send_message_form(
                                    client,
                                    &token,
                                    channel_id,
                                    "Here is an image of Leo",
                                    "images/leo_slander.jpg",
                                )
                                .await
                                {
                                    Ok(_) => println!("Image of Leo sent!"),
                                    Err(e) => eprintln!("Failed to send image: {}", e),
                                }
                            }

                            if content.starts_with("!askleo randomleo") {
                                println!("Random Picture of Leo asked for");
                                let client = DiscordHTTP::new();
                                match DiscordHTTP::send_message_form(
                                    client,
                                    &token,
                                    channel_id,
                                    "Here is an image of Leo",
                                    "images/leo_slander.jpg",
                                )
                                .await
                                {
                                    Ok(_) => println!("Image of Leo sent!"),
                                    Err(e) => eprintln!("Failed to send image: {}", e),
                                }
                            }

                            if content.starts_with("!askleo leave") {
                                // Implement this later
                            }
                        }

                        Some(specifier) => {
                            println!("Unhandled event specifier: {}", specifier);
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
