mod gateway;

#[tokio::main]
async fn main() {
    let _ =  match gateway::discord::gateway_connect().await {
        Ok(_) => println!("Gateway connected"),
        Err(e) => println!("Gateway connection failed: {:?}", e),
    };
}
