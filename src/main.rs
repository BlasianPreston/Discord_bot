use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use futures_util::{SinkExt, StreamExt};
mod gateway;

use gateway::GatewayConnection;

fn main() {
    println!("Hello, world!");
}
