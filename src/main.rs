use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use futures_util::{SinkExt, StreamExt};
mod gateway;

#[tokio::main]
async fn main() {
    println!("Hello, world!");
}
