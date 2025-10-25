use serde::{Deserialize, Serialize};
use serde_with::{serde_as, VecSkipError};
use tokio_tungstenite::tungstenite::Message;

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "op", content = "d")]
pub enum Payload {
    #[serde(rename = "0")]
    Ready(Ready),

    #[serde(rename = "1")]
    ImmediateHeartbeat(ImmediateHeartbeat),

    #[serde(rename = "2")]
    Ready(Ready),

    #[serde(rename = "10")]
    Hello(Hello),

    #[serde(rename = "11")]
    HeartbeatAck(HeartbeatAck)
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Identify {
    pub server_id: String,
    pub user_id: String,
    pub session_id: String,
    pub token: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ImmediateHeartbeat { // Work on heartbeat, need to send d value

}

#[derive(Serialize, Deserialize, Debug)]
pub struct Ready {
    pub token : String,
    pub properties: ReadyProperties
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ReadyProperties {
    pub os : String,
    pub browser : String,
    pub device : String
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Hello {
    pub heartbeat_interval: u64,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct HeartbeatAck {
    pub heartbeat_interval: u64,
}