use crate::threaded_rtp_handler::rtp_connection::{RtpConnection, RtpConnectionHandle};
use crate::threaded_rtp_handler::udp_consumer::UdpConsumer;
use crate::threaded_rtp_handler::StreamMap;
use crate::IncomingStreamHandle;
use dashmap::DashMap;
use staaf_core::rpc::MediaStream;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::thread::{spawn, JoinHandle};

pub struct IngestManager {
    stream_map: StreamMap,
    socket_map: HashMap<u16, UdpSocket>,
    udp_consumers: Vec<JoinHandle<()>>,
}

impl IngestManager {
    pub fn new() -> IngestManager {
        let stream_map = Arc::new(DashMap::new());
        IngestManager {
            stream_map,
            socket_map: Default::default(),
            udp_consumers: vec![],
        }
    }

    pub fn spawn_socket(&mut self) {
        let mut consumer = UdpConsumer::new();
        let socket = consumer
            .socket()
            .try_clone()
            .expect("Failed to clone socket");
        consumer.spawn_handlers(&self.stream_map);
        let handle = spawn(move || consumer.run());
        self.udp_consumers.push(handle);
        self.socket_map
            .insert(socket.local_addr().unwrap().port(), socket);
    }

    pub fn register_stream(
        &mut self,
        stream_index: u32,
        ip_addr: IpAddr,
        port: u16,
        media_streams: Vec<MediaStream>,
    ) {
        let socket = self
            .socket_map
            .get(&port)
            .expect(&format!(
                "No UdpConsumer on port {} even though requested",
                port
            ))
            .try_clone()
            .expect("Failed to clone socket");
        let mut rtp_connection = RtpConnection::new(socket, media_streams.clone(), stream_index);
        for stream in media_streams {
            self.stream_map.insert(
                IncomingStreamHandle {
                    ssrc: stream.ssrc,
                    incoming_port: port,
                    peer_addr: ip_addr,
                },
                rtp_connection.handle(),
            );
        }

        spawn(move || rtp_connection.run());
    }
}
