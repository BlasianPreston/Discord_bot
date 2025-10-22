use reqwests::{Client, header};
use futures_util::{StreamExt, SinkExt};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use std::env;

/* 
    {
  "op": 0, 	integer	Gateway opcode, which indicates the payload type
  "d": {}, ?mixed (any JSON value)	Event data
  "s": 42, ?integer *	Sequence number of event used for resuming sessions and heartbeating
  "t": "GATEWAY_EVENT_NAME" 	?string *	Event name
    }
*/

pub struct GatewayConnection {
    tx_outbound: mpsc::Sender<String>,
    rx_inbound: mpsc::Receiver<String>,
    sender_task: tokio::task::JoinHandle<()>,
    receiver_task: tokio::task::JoinHandle<()>
}

impl GatewayConnection {
    pub async fn connect() -> anyhow::Result<Self> {
        // Connect to the Discord gateway
        let url = Url::parse("wss://gateway.discord.gg/?v=10&encoding=json")?;
        let (ws_stream, _) = connect_async(url).await?;
        println!("Connected to Discord gateway!");

        let (mut write, mut read) = ws_stream.split();

        // Create communication channels
        let (tx_outbound, mut rx_outbound) = mpsc::channel::<String>(32);
        let (tx_inbound, rx_inbound) = mpsc::channel::<String>(32);

        let sender_task = tokio::spawn(async move {
            while let Some(msg) = rx_outbound.recv().await {
                if let Err(e) = write.send(Message::Text(msg)).await {
                    eprintln!("Error sending message: {e}");
                    break;
                }
            }
            println!("Sender task ended.");
        });

        let receiver_task = tokio::spawn(async move {
            while let Some(Ok(msg)) = read.next().await {
                if let Message::Text(text) = msg {
                    if let Err(e) = tx_inbound.send(text).await {
                        eprintln!("Error forwarding inbound message: {e}");
                        break;
                    }
                }
            }
            println!("Receiver task ended.");
        });

        println!("Gateway connection established!");

        Ok(Self {
            tx_outbound,
            rx_inbound,
            sender_task,
            receiver_task,
        })
    }
    pub async fn send(&self, msg: String) -> anyhow::Result<()> {
        self.tx_outbound.send(msg).await?;
        Ok(())
    }

    pub async fn recv(&mut self) -> Option<String> {
        self.rx_inbound.recv().await
    }
}


#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenv().ok();
    let token = env::var("DISCORD_TOKEN");

    let mut gateway = GatewayConnection::connect().await?;

    let identify = serde_json::json!({
        "op": 2,
        "d": {
            "token": token,
            "intents": 513, // example: GUILDS + GUILD_MESSAGES
            "properties": { "$os": "linux", "$browser": "rust-bot", "$device": "rust-bot" }
        }
    });

    gateway.send(identify.to_string()).await?;

    // Example: listen for gateway messages
    while let Some(msg) = gateway.recv().await {
        println!("Got message: {}", msg);
    }

    Ok(())
}
