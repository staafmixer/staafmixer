mod manager;
mod message_handler;
mod rtp_connection;
mod rtp_utils;
mod udp_consumer;

use crate::threaded_rtp_handler::manager::IngestManager;
use crate::threaded_rtp_handler::rtp_connection::RtpConnectionHandle;
use crate::threaded_rtp_handler::rtp_utils::MuxedRtp;
use crate::{get_free_port, IncomingStreamHandle};
use bytes::{Bytes, BytesMut};
use crossbeam_channel::{bounded, Receiver, Sender};
use dashmap::DashMap;
use lazy_static::lazy_static;
use staaf_core::rpc::MediaStream;
use std::mem;
use std::mem::MaybeUninit;
use std::net::{IpAddr, Ipv6Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::thread::{spawn, JoinHandle};

const HANDLERS_PER_SOCKET: usize = 5;
const UDP_MESSAGE_QUEUE_DEPTH: usize = 100;
const RTP_MESSAGE_QUEUE_DEPTH: usize = 100;
const MAX_AMOUNT_OF_MUXED_RTP_STREAMS: usize = 5;

lazy_static! {
    pub static ref INGEST_MANAGER: RwLock<IngestManager> = RwLock::new(IngestManager::new());
}

pub type StreamMap = Arc<DashMap<IncomingStreamHandle, Arc<RtpConnectionHandle>>>;

pub fn start_ingest() {
    INGEST_MANAGER.write().unwrap().spawn_socket();
}

pub struct UdpMessage {
    pub address: SocketAddr,
    pub data: Bytes,
}

pub struct UdpRtpMessage {
    pub address: SocketAddr,
    pub data: Bytes,
    pub rtp: MuxedRtp,
}
