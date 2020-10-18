use crate::StreamId;
use serde::{Deserialize, Serialize};
use std::net::IpAddr;

#[derive(Serialize, Deserialize, Debug, Copy, Clone)]
pub struct MediaStream {
    pub ssrc: u32,
    pub payload_type: u16,
    pub content: MediaStreamContent,
}

#[derive(Serialize, Deserialize, Debug, Copy, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MediaStreamContent {
    Video(MediaStreamVideo),
    Audio(MediaStreamAudio),
}

#[derive(Serialize, Deserialize, Debug, Copy, Clone)]
pub enum AudioCodec {
    Opus,
}

impl AudioCodec {
    pub fn to_str(&self) -> &'static str {
        match self {
            AudioCodec::Opus => "opus",
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Copy, Clone)]
pub enum VideoCodec {
    H264,
    VP9,
}

impl VideoCodec {
    pub fn to_str(&self) -> &'static str {
        match self {
            VideoCodec::VP9 => "VP9",
            VideoCodec::H264 => "H264",
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Copy, Clone)]
pub struct MediaStreamAudio {
    pub codec: AudioCodec,
}

#[derive(Serialize, Deserialize, Debug, Copy, Clone)]
pub struct MediaStreamVideo {
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StaafRPC {
    RegisterStream(RegisterStream),
    StreamRegistered { port: u16 },
    Subscribe { stream_id: StreamId },
    Unsubscribe { stream_id: StreamId },
    Offer,
    Answer,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RegisterStream {
    pub media_streams: Vec<MediaStream>,
    pub peer_addr: IpAddr,
    pub stream_id: StreamId,
}
