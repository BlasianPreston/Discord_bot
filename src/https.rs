use reqwests::{Client, header};
use anyhow

pub struct DiscordHTTPS {
    client: Client,
    token: String
}

impl DiscordHTTPS {
    pub fn new(token: String) -> Self {
        let client = Client::new();
        Self {client, token}
    }

    pub async fn send_messgae(&self, channel_id : &str, content : &str) -> anyhow::Result<()> {
        let url = format!("https://discord.com/api/v10/channels/{}/messages", channel_id); // Channel ID: 1206995023929937923
        let body = serde_json::json!({"content": content});

        self.client.post(&url).header(header::AUTHORIZATION, format!("Bot {}", self.token)).json(&body).send().await?.error_for_status()?; // Sends authorization to discord

        Ok(())
    }
}