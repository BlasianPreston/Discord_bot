mod gateway;

#[tokio::main]
async fn main() {
    let _ = gateway::discord::gateway_connect().await;
}
