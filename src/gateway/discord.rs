use crate::gateway::voice;

use super::https::DiscordHTTP;
use anyhow::Result;
use dotenv::dotenv;
use futures_util::{SinkExt, stream::StreamExt};
use rand::Rng;
use serde_json::Value;
use std::collections::HashMap;
use std::{env, time::Duration};
use tokio::select;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};

pub fn get_random_number(start: u64, end: u64) -> u64 {
    let mut rng = rand::rng();
    rng.random_range(start..=end)
}

pub async fn gateway_connect() -> Result<()> {
    let url = "wss://gateway.discord.gg/?v=10&encoding=json";
    let (ws_stream, _) = connect_async(url).await?;
    println!("Connected to Discord gateway!");

    let (write, mut read) = ws_stream.split();

    // Send info through tx_outbound and receive it through rx_outbound which sends it to write which sends it to discord
    // Increased buffer size to prevent blocking heartbeats
    let (tx_outbound, mut rx_outbound) = mpsc::channel::<String>(256);

    // Anything we want to handle ourselves and then send back to discord, send through tx_inbound which is received by rx_inbound
    let (tx_inbound, mut rx_inbound) = mpsc::channel::<Value>(32);

    // Send s value to heartbeat
    let (seq_tx, seq_rx) = tokio::sync::watch::channel(None::<u64>);

    // Send values to reconnect
    let (rec_tx, rec_rx) = tokio::sync::watch::channel((None::<String>, None::<String>));
    // Store session_id, user_id, and voice server info for voice connections
    let (session_tx, session_rx) = tokio::sync::watch::channel(None::<String>);
    let (user_id_tx, user_id_rx) = tokio::sync::watch::channel(None::<String>);
    let (voice_server_tx, voice_server_rx) =
        tokio::sync::watch::channel(None::<(String, String, String)>); // (endpoint, token, guild_id)
    // Track when bot joins voice channels (guild_id -> channel_id)
    let (bot_voice_state_tx, bot_voice_state_rx) =
        tokio::sync::watch::channel(None::<(String, Option<String>)>); // (guild_id, channel_id)

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
        let mut write = write; // Move write into the closure
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
                                // Send message to WebSocket (non-blocking where possible)
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

                                // Send Identify (non-blocking)
                                let _ = tx_outbound_clone.send(identify.to_string()).await;
                                println!("Sent Identify payload.");

                                // Spawn the heartbeat task after receiving the interval
                                let tx_heartbeat = tx_outbound_clone.clone();
                                let seq_rx_heartbeat = seq_rx_clone.clone();
                                tokio::spawn(async move {
                                    loop {
                                        // Wait for the interval first
                                        tokio::time::sleep(Duration::from_millis(interval_ms)).await;

                                        // Get current sequence number (may be None initially, which is fine)
                                        let seq_val = *seq_rx_heartbeat.borrow();
                                        let heartbeat =
                                            serde_json::json!({ "op": 1, "d": seq_val }).to_string();

                                        // Send heartbeat (non-blocking, don't wait if channel is full)
                                        if tx_heartbeat.try_send(heartbeat).is_err() {
                                            eprintln!("Heartbeat channel full, skipping heartbeat");
                                        } else {
                                            println!("Sent heartbeat with sequence: {:?}", seq_val);
                                        }
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

            match json["op"].as_u64() {
                Some(0) => {
                    let event_name = json["t"].as_str();
                    match event_name {
                        Some("READY") => {
                            let d_content = &json["d"];
                            let mut session_id = None;
                            let mut resume_gateway_url = None;
                            if let Some(id) = d_content["session_id"].as_str() {
                                session_id = Some(id.to_string());
                                let _ = session_tx.send(Some(id.to_string()));
                            }
                            if let Some(url) = d_content["resume_gateway_url"].as_str() {
                                resume_gateway_url = Some(url.to_string());
                            }
                            if let Some(user) = d_content["user"]["id"].as_str() {
                                let _ = user_id_tx.send(Some(user.to_string()));
                            }
                            let _ = rec_tx.send((session_id, resume_gateway_url));
                        }

                        Some("GUILD_CREATE") => {
                            let d_content = &json["d"];
                            if let Some(voice_states) = d_content["voice_states"].as_array() {
                                for voice_state in voice_states {
                                    let user_id = voice_state["user_id"]
                                        .as_str()
                                        .unwrap_or_default()
                                        .to_string();
                                    let channel_id = match voice_state["channel_id"].as_str() {
                                        Some(id) => Some(id.to_string()),
                                        None => None,
                                    };
                                    users_to_channels.insert(user_id, channel_id);
                                }
                            }
                        }

                        Some("VOICE_STATE_UPDATE") => {
                            let d_content = &json["d"];
                            let channel_id = match d_content["channel_id"].as_str() {
                                Some(id) => Some(id.to_string()),
                                None => None,
                            };
                            let user_id = d_content["user_id"].as_str().unwrap_or("").to_string();
                            let guild_id = d_content["guild_id"].as_str().unwrap_or("").to_string();
                            users_to_channels.insert(user_id.clone(), channel_id.clone());

                            // Check if this is the bot's own voice state update
                            let bot_id = env::var("DISCORD_APP_ID")
                                .ok()
                                .and_then(|id| id.parse::<String>().ok())
                                .unwrap_or_default();
                            println!(
                                "VOICE_STATE_UPDATE - user_id: {}, bot_id: {}, match: {}",
                                user_id,
                                bot_id,
                                user_id == bot_id
                            );
                            if user_id == bot_id {
                                println!(
                                    "Bot voice state update - guild: {}, channel: {:?}",
                                    guild_id, channel_id
                                );
                                let _ = bot_voice_state_tx.send(Some((guild_id, channel_id)));
                            }
                        }

                        Some("VOICE_SERVER_UPDATE") => {
                            let d_content = &json["d"];
                            let guild_id = d_content["guild_id"].as_str().unwrap_or("").to_string();

                            // Check if endpoint is null (JSON null, not string)
                            if d_content["endpoint"].is_null() {
                                println!(
                                    "VOICE_SERVER_UPDATE: endpoint is null (voice server not ready yet), waiting for next update"
                                );
                                continue;
                            }

                            if let Some(endpoint) = d_content["endpoint"].as_str() {
                                if !endpoint.is_empty() {
                                    if let Some(token) = d_content["token"].as_str() {
                                        println!(
                                            "VOICE_SERVER_UPDATE received for guild {} - endpoint: {}, token: {}...",
                                            guild_id,
                                            endpoint,
                                            if token.len() > 10 {
                                                &token[..10]
                                            } else {
                                                token
                                            }
                                        );
                                        let _ = voice_server_tx.send(Some((
                                            endpoint.to_string(),
                                            token.to_string(),
                                            guild_id,
                                        )));
                                    } else {
                                        eprintln!("VOICE_SERVER_UPDATE: token is missing");
                                    }
                                } else {
                                    println!(
                                        "VOICE_SERVER_UPDATE: endpoint is empty, waiting for next update"
                                    );
                                }
                            } else {
                                println!(
                                    "VOICE_SERVER_UPDATE: endpoint field is missing or invalid type"
                                );
                            }
                        }

                        Some("MESSAGE_CREATE") => {
                            let d_content = json["d"].clone();
                            let bot_id = env::var("DISCORD_APP_ID")
                                .expect("DISCORD_APP_ID not set")
                                .parse()
                                .unwrap_or(0)
                                .to_string();
                            let content = d_content["content"].as_str().unwrap_or("");
                            let author_id =
                                d_content["author"]["id"].as_str().unwrap_or("").to_string();
                            let channel_id = d_content["channel_id"].as_str().unwrap_or("");
                            let guild_id = d_content["guild_id"].as_str().unwrap_or("");

                            // Ignore your own messages
                            if author_id == (bot_id) {
                                continue;
                            }

                            if content.starts_with("!askleo help") {
                                println!("User asked for help");
                                let help_message = "**Leo Cap-lan Bot Commands:**\n\n\
                                    `!askleo play <youtube_url>` - Play audio from a YouTube video in the voice channel\n\
                                    `!askleo leave` - Leave the current voice channel\n\
                                    `!askleo slanderleo` - Send the slander image of Leo\n\
                                    `!askleo randomleo` - Get a random image of Leo\n\n\
                                    Use `!askleo help` to see this message again.";

                                // Spawn HTTP request to avoid blocking event handler
                                let token_help = token.clone();
                                let channel_id_help = channel_id.to_string();
                                tokio::spawn(async move {
                                    let client = DiscordHTTP::new();
                                    if let Err(e) = DiscordHTTP::send_message_text(
                                        client,
                                        &token_help,
                                        &channel_id_help,
                                        help_message,
                                    )
                                    .await
                                    {
                                        eprintln!("Failed to send help message: {}", e);
                                    }
                                });
                            }

                            if content.starts_with("!askleo play") {
                                let voice_channel = users_to_channels.get(&author_id);
                                let prefix = "!askleo play ";
                                if let Some(link) = content.strip_prefix(prefix) {
                                    let link = link.to_string(); // Clone the link
                                    println!("Extracted link: {}", link);
                                    let join_json = serde_json::json!({
                                      "op": 4,
                                      "d": {
                                        "guild_id": guild_id,
                                        "channel_id": voice_channel,
                                        "self_mute": false,
                                        "self_deaf": false
                                      }
                                    });
                                    // Use try_send to avoid blocking the event handler
                                    let tx_outbound_play = tx_outbound.clone();
                                    let join_msg = join_json.to_string();
                                    if tx_outbound_play.try_send(join_msg).is_err() {
                                        println!("Error sending voice join message (channel full)");
                                    } else {
                                        println!("Successfully queued voice channel join");
                                    }

                                    // Wait for voice server info and session/user info
                                    let mut session_rx_clone = session_rx.clone();
                                    let mut user_id_rx_clone = user_id_rx.clone();
                                    let mut voice_server_rx_clone = voice_server_rx.clone();
                                    let mut bot_voice_state_rx_clone = bot_voice_state_rx.clone();
                                    let link_clone = link.clone();
                                    let guild_id_clone = guild_id.to_string();
                                    tokio::spawn(async move {
                                        println!("Waiting for voice connection info...");

                                        // Wait for or get existing values
                                        if session_rx_clone.borrow().is_none() {
                                            println!("Waiting for session_id...");
                                            session_rx_clone.changed().await.ok();
                                        }
                                        if user_id_rx_clone.borrow().is_none() {
                                            println!("Waiting for user_id...");
                                            user_id_rx_clone.changed().await.ok();
                                        }

                                        // Wait for bot's VOICE_STATE_UPDATE to confirm it joined (with timeout)
                                        println!("Waiting for bot to join voice channel...");
                                        // Check if bot is already in the channel
                                        let mut bot_joined = false;
                                        if let Some((g, ch)) =
                                            bot_voice_state_rx_clone.borrow().as_ref()
                                        {
                                            if g == &guild_id_clone && ch.is_some() {
                                                println!(
                                                    "Bot already confirmed in voice channel for guild {}",
                                                    guild_id_clone
                                                );
                                                bot_joined = true;
                                            }
                                        }

                                        // Wait for update if not already joined (with 5 second timeout)
                                        if !bot_joined {
                                            let timeout =
                                                tokio::time::sleep(Duration::from_secs(5));
                                            tokio::pin!(timeout);

                                            loop {
                                                tokio::select! {
                                                    _ = &mut timeout => {
                                                        println!("Timeout waiting for bot voice state update, proceeding anyway...");
                                                        break;
                                                    }
                                                    _ = bot_voice_state_rx_clone.changed() => {
                                                        if let Some((g, ch)) = bot_voice_state_rx_clone.borrow().as_ref() {
                                                            if g == &guild_id_clone && ch.is_some() {
                                                                println!("Bot confirmed in voice channel for guild {}", guild_id_clone);
                                                                break;
                                                            }
                                                        }
                                                        println!("Still waiting for bot voice state update...");
                                                    }
                                                }
                                            }
                                        }

                                        // Wait for VOICE_SERVER_UPDATE for this specific guild
                                        loop {
                                            if let Some((_e, _t, g)) =
                                                voice_server_rx_clone.borrow().as_ref()
                                            {
                                                if g == &guild_id_clone {
                                                    println!(
                                                        "Found voice server info for guild {}",
                                                        guild_id_clone
                                                    );
                                                    break;
                                                }
                                            }
                                            println!(
                                                "Waiting for VOICE_SERVER_UPDATE for guild {}...",
                                                guild_id_clone
                                            );
                                            voice_server_rx_clone.changed().await.ok();
                                        }

                                        // Additional delay to ensure everything is ready
                                        println!(
                                            "All voice info received, waiting 2 seconds before connecting..."
                                        );
                                        tokio::time::sleep(Duration::from_secs(2)).await;

                                        let session_id = match session_rx_clone.borrow().as_ref() {
                                            Some(s) => {
                                                println!("Got session_id: {}", s);
                                                s.clone()
                                            }
                                            None => {
                                                eprintln!("Session ID not available");
                                                return;
                                            }
                                        };

                                        let user_id = match user_id_rx_clone.borrow().as_ref() {
                                            Some(u) => {
                                                println!("Got user_id: {}", u);
                                                u.clone()
                                            }
                                            None => {
                                                eprintln!("User ID not available");
                                                return;
                                            }
                                        };

                                        let (endpoint, voice_token, guild_id_voice) =
                                            match voice_server_rx_clone.borrow().as_ref() {
                                                Some((e, t, g)) => {
                                                    println!(
                                                        "Got voice server info - endpoint: {}, guild: {}",
                                                        e, g
                                                    );
                                                    (e.clone(), t.clone(), g.clone())
                                                }
                                                None => {
                                                    eprintln!("Voice server info not available");
                                                    return;
                                                }
                                            };

                                        println!("Connecting to voice server...");
                                        if let Err(e) = voice::connect_to_voice_server(
                                            guild_id_voice,
                                            endpoint,
                                            voice_token,
                                            session_id,
                                            user_id,
                                            link_clone,
                                        )
                                        .await
                                        {
                                            eprintln!("Failed to connect to voice server: {}", e);
                                        } else {
                                            println!("Successfully connected to voice server!");
                                        }
                                    });
                                } else {
                                    println!("Input did not match the command prefix.");
                                    let client = DiscordHTTP::new();
                                    if let Err(e) = DiscordHTTP::send_message_text(
                                        client,
                                        &token,
                                        channel_id,
                                        "Invalid link was sent",
                                    )
                                    .await
                                    {
                                        eprintln!("Failed to send error message: {}", e);
                                    }
                                }
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
                                let image_num = get_random_number(1, 21);
                                match DiscordHTTP::send_message_form(
                                    client,
                                    &token,
                                    channel_id,
                                    "Here is an image of Leo",
                                    format!("images/leo_{}.jpg", image_num).as_str(),
                                )
                                .await
                                {
                                    Ok(_) => println!("Image of Leo sent!"),
                                    Err(e) => eprintln!("Failed to send image: {}", e),
                                }
                            }

                            if content.starts_with("!askleo leave") {
                                let join_json = serde_json::json!({
                                  "op": 4,
                                  "d": {
                                    "guild_id": guild_id,
                                    "channel_id": null,
                                    "self_mute": false,
                                    "self_deaf": false
                                  }
                                });
                                if tx_outbound.send(join_json.to_string()).await.is_err() {
                                    println!("Error sending message");
                                }
                                println!("Successfully left voice channel");
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
