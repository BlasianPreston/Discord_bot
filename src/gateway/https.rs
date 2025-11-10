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

    pub fn make_image_payload(
        text: &str,
        image_path: &str,
    ) -> Result<multipart::Form, std::io::Error> {
        let payload_json = json!({
            "content": text
        });

        let file_bytes = fs::read(image_path)?;
        let file_name = std::path::Path::new(image_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("image.png");

        let part = multipart::Part::bytes(file_bytes)
            .file_name(file_name.to_string())
            .mime_str("image/png")
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

        let form = multipart::Form::new()
            .text("payload_json", payload_json.to_string())
            .part("files[0]", part);

        Ok(form)
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

    pub async fn send_message_form(
        client: Self,
        token: &str,
        channel_id: &str,
        content: multipart::Form,
    ) -> Result<(), reqwest::Error> {
        let url = format!(
            "https://discord.com/api/v10/channels/{}/messages",
            channel_id
        );

        client
            .client
            .post(&url)
            .bearer_auth(token)
            .multipart(content)
            .send()
            .await?;

        Ok(())
    }
}
