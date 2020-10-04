use crate::StreamId;
use serde::{Deserialize, Serialize};
use std::net::IpAddr;

#[derive(Serialize, Deserialize, Debug, Copy, Clone)]
pub struct MediaStream {
    pub ssrc: u32,
    pub payload_type: u16,
    pub is_video: bool,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum StaafRPC {
    RegisterStream(RegisterStream),
    Subscribe(StreamId),
    Unsubscribe(StreamId),
    Offer,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct RegisterStream {
    pub media_streams: Vec<MediaStream>,
    pub peer_addr: IpAddr,
    pub stream_id: StreamId,
}
