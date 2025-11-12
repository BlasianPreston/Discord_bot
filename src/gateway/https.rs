use anyhow::{Context, Result};
use reqwest::Client;
use reqwest::multipart;
use serde_json::json;
use std::fs;

pub struct DiscordHTTP {
    client: Client,
}

impl DiscordHTTP {
    pub fn new() -> Self {
        Self {
            client: Client::new(),
        }
    }

    pub async fn _send_message_text(
        client: Self,
        token: &str,
        channel_id: &str,
        content: &str,
    ) -> Result<(), reqwest::Error> {
        let url = format!(
            "https://discord.com/api/v10/channels/{}/messages",
            channel_id
        );
        let body = json!({ "content": content });

        client
            .client
            .post(&url)
            .bearer_auth(token)
            .json(&body)
            .send()
            .await?;

        Ok(())
    }

    fn make_image_payload(text: &str, image_path: &str) -> Result<multipart::Form, std::io::Error> {
        let payload_json = json!({
            "content": text
        });

        let file_bytes = fs::read(image_path)?;
        let file_name = std::path::Path::new(image_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("image.png");

        // Determine MIME type based on file extension
        let mime_type = std::path::Path::new(image_path)
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| match ext.to_lowercase().as_str() {
                "jpg" | "jpeg" => "image/jpeg",
                "png" => "image/png",
                "gif" => "image/gif",
                "webp" => "image/webp",
                _ => "image/png",
            })
            .unwrap_or("image/png");

        let part = multipart::Part::bytes(file_bytes)
            .file_name(file_name.to_string())
            .mime_str(mime_type)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

        let form = multipart::Form::new()
            .text("payload_json", payload_json.to_string())
            .part("files[0]", part);

        Ok(form)
    }

    pub async fn send_message_form(
        client: Self,
        token: &str,
        channel_id: &str,
        text: &str,
        image_path: &str,
    ) -> Result<()> {
        let url = format!(
            "https://discord.com/api/v10/channels/{}/messages",
            channel_id
        );

        let form = DiscordHTTP::make_image_payload(text, image_path)
            .with_context(|| format!("Failed to read image file: {}", image_path))?;

        let response = client
            .client
            .post(&url)
            .header("Authorization", format!("Bot {}", token))
            .multipart(form)
            .send()
            .await
            .context("Failed to send HTTP request to Discord API")?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            eprintln!("Discord API error: {} - {}", status, error_text);
            anyhow::bail!(
                "Discord API returned error status: {} - {}",
                status,
                error_text
            );
        }

        Ok(())
    }
}
