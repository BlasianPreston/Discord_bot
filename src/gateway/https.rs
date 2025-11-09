use reqwest::{Client, RequestBuilder};
use serde_json::json;

async fn send_message(token: &str, channel_id: &str, content: &str) -> Result<(), reqwest::Error> {
    let client = Client::new();
    let url = format!(
        "https://discord.com/api/v10/channels/{}/messages",
        channel_id
    );
    let body = json!({ "content": content });

    client
        .post(&url)
        .bearer_auth(token)
        .json(&body)
        .send()
        .await?;

    Ok(())
}
