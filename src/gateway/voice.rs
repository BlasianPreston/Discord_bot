use anyhow::Result;
use futures_util::{SinkExt, stream::StreamExt};
use serde_json::{Value, json};
use tokio::select;
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;

struct VoiceGateway {
    guild_id: String,
    endpoint: String,
    token: String,
    session_id: String,
    user_id: String,
    ws_handle: String,  // Update typing
    udp_socket: String, // Update typing
    secret_key: Option<[u8; 32]>,
}

impl VoiceGateway {
    // Create join voice gateway function
    async fn connect_to_voice_server(
        guild_id: &str,
        endpoint: &str,
        token: &str,
        session_id: &str,
        user_id: &str,
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

        tokio::spawn(async move {
            loop {
                select! {
                    msg = rx_outbound.recv() => {
                        if let Some(msg) = msg {
                            let msg_json: Value = serde_json::from_str(&msg).unwrap_or_default();
                            match msg_json["op"].as_u64() { // We need to send out op codes 0 and 1
                                Some(2) => {}
                                Some(4) => {}
                                Some(8) => {}
                                Some(_) => {}
                                None => {println!("Invalid Json sent to tx_outbound")}
                        }
                        }
                    }
                }
            }
        });

        Ok(())
    }
}
