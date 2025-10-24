use dotenv::dotenv;
use futures_util::{SinkExt, StreamExt};
use reqwest::{Client, Url, header};
use serde_json::Value;
use std::env;
use tokio::sync::mpsc;
use tokio::time::Duration;
use std::sync::{Arc, Mutex} 
use tokio_tungstenite::{connect_async, tungstenite::Message};

/*
    {
  "op": 0, 	integer	Gateway opcode, which indicates the payload type
  "d": {}, ?mixed (any JSON value)	Event data
  "s": 42, ?integer *	Sequence number of event used for resuming sessions and heartbeating
  "t": "GATEWAY_EVENT_NAME" 	?string *	Event name
    }
*/

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let url = "wss://gateway.discord.gg/?v=10&encoding=json";
    let (ws_stream, _) = connect_async(url).await?;
    let ws_stream = Arc::new(Mutex::new(ws_stream)); // Finish cloning ws_stream
    let ws_clone_for_read = Arc::clone(&ws_stream);
    let ws_clone_for_heartbeat = Arc::clone(&ws_stream);
    let ws_clone_for_sender = Arc::clone(&ws_stream);
    println!("Connected to Discord gateway!");

    let (mut write, mut read) = ws_stream.split();

    // Create communication channels
    let (tx_outbound, rx_outbound) = mpsc::channel::<String>(32);
    let (tx_inbound, rx_inbound) = mpsc::channel::<String>(32);
    let (interval_tx, mut interval_rx) = tokio::sync::mpsc::channel::<u64>(1);

    dotenv().ok();
    let token = env::var("DISCORD_TOKEN")?;
    let identify = serde_json::json!({
        "op": 2,
        "d": {
            "token": token,
            "intents": 513, // example: GUILDS + GUILD_MESSAGES
            "properties": { "$os": "linux", "$browser": "rust-bot", "$device": "rust-bot" }
        }
    });

    tokio::spawn(async move {
        while let Some(msg) = read.next().await {
            if let Ok(Message::Text(text)) = msg {
                let json: Value = serde_json::from_str(&text).unwrap();
                if json["op"] == 10 {
                    let interval = json["d"]["heartbeat_interval"].as_u64().unwrap();
                    interval_tx.send(interval).await.unwrap();
                }
            }
        }
    });

    let interval = interval_rx.recv().await.unwrap();
    println!("Received heartbeat interval: {} ms", interval);
    write.send(Message::Text(identify.to_string().into()));

    let write_clone = write.clone();
    let mut inteval_task = tokio::spawn(async move {
        loop {
            inteval_task
                .send(Message::Text(
                    serde_json::json!({ "op": 1, "d": null }).to_string().into(),
                ))
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(interval)).await;
        }
    });

    let sender_task = tokio::spawn(async move {
        while let Some(msg) = rx_outbound.recv().await {
            if let Err(e) = write.send(Message::Text(msg.into())).await {
                eprintln!("Error sending message: {e}");
                break;
            }
        }
        println!("Sender task ended.");
    });

    let receiver_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = read.next().await {
            if let Message::Text(text) = msg {
                if let Err(e) = tx_inbound.send(text.to_string()).await {
                    eprintln!("Error forwarding inbound message: {e}");
                    break;
                }
            }
        }
        println!("Receiver task ended.");
    });

    println!("Gateway connection established!");

    while let Some(msg) = rx_inbound.recv().await {
        let mut interval = 0;
        if let Ok(json) = serde_json::from_str::<Value>(&msg) {
            if interval != 0 {
            } else if json["op"] == 10 {
                interval = json["d"]["heartbeat_interval"]
                    .as_u64()
                    .expect("missing heartbeat_interval");
                println!("Got heartbeat interval: {} ms", interval);
                break;
            }
        } else {
            eprintln!("Failed to parse message: {}", msg);
        }
    }

    Ok(())
}
