use crate::threaded_rtp_handler::rtp_connection::RtpConnectionHandle;
use crate::threaded_rtp_handler::rtp_utils::MuxedRtp;
use crate::threaded_rtp_handler::udp_consumer::UdpConsumer;
use crate::threaded_rtp_handler::{StreamMap, UdpMessage, UdpRtpMessage};
use crate::IncomingStreamHandle;
use crossbeam_channel::Receiver;
use dashmap::DashMap;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::Ordering;
use std::sync::Arc;

pub struct MessageHandler {
    socket: UdpSocket,
    bound_port: u16,
    queue: Receiver<UdpMessage>,
    stream_map: StreamMap,
}

impl MessageHandler {
    pub fn handler_for(consumer: &UdpConsumer, stream_map: StreamMap) -> MessageHandler {
        let socket = consumer
            .socket()
            .try_clone()
            .expect("Failed to clone UDP Socket");
        MessageHandler {
            bound_port: socket.local_addr().unwrap().port(),
            socket,
            queue: consumer.receiver(),
            stream_map,
        }
    }

    pub fn run(&mut self) {
        for message in self.queue.iter() {
            let muxed_rtp = if let Some(muxed_rtp) = MuxedRtp::parse(&message.data) {
                muxed_rtp
            } else {
                continue;
            };
            if let Some(stream_handle) = self.stream_map.get(&IncomingStreamHandle {
                ssrc: muxed_rtp.ssrc(),
                incoming_port: self.bound_port,
                peer_addr: message.address.ip(),
            }) {
                if let Err(_) = stream_handle.queue.send(UdpRtpMessage {
                    address: message.address,
                    data: message.data,
                    rtp: muxed_rtp,
                }) {
                    println!("Dropped packet for stream {}", stream_handle.stream_index);
                    // Do some analytics maybe or smth
                    stream_handle.drop_counter.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }
}
