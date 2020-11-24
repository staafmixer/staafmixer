use crate::threaded_rtp_handler::message_handler::MessageHandler;
use crate::threaded_rtp_handler::rtp_connection::RtpConnectionHandle;
use crate::threaded_rtp_handler::{
    StreamMap, UdpMessage, HANDLERS_PER_SOCKET, UDP_MESSAGE_QUEUE_DEPTH,
};
use bytes::BytesMut;
use crossbeam_channel::{bounded, Receiver, Sender};
use net2::UdpSocketExt;
use std::iter::FromIterator;
use std::mem;
use std::net::{IpAddr, Ipv6Addr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::thread::{spawn, JoinHandle};

pub struct UdpConsumer {
    socket: UdpSocket,
    queue: Sender<UdpMessage>,
    queue_receiver: Receiver<UdpMessage>,
    handlers: Vec<JoinHandle<()>>,
}

impl UdpConsumer {
    pub fn new() -> UdpConsumer {
        let socket =
            UdpSocket::bind(SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 1445)).unwrap();

        // 10 MB recv buffer
        socket.set_recv_buffer_size(10_000_000).unwrap();

        let (sender, recv) = bounded(UDP_MESSAGE_QUEUE_DEPTH);
        UdpConsumer {
            socket,
            queue: sender,
            queue_receiver: recv,
            handlers: vec![],
        }
    }

    pub fn spawn_handlers(&mut self, stream_map: &StreamMap) {
        for _ in 0..HANDLERS_PER_SOCKET {
            let mut handler = MessageHandler::handler_for(&self, Arc::clone(&stream_map));
            let handle = spawn(move || {
                handler.run();
            });
            self.handlers.push(handle);
        }
    }

    pub fn run(&mut self) {
        let mut drop_counter: u64 = 0;
        let mut buffer = BytesMut::new();
        buffer.resize(65536, 0);
        while let Ok((size, address)) = self.socket.recv_from(&mut buffer) {
            buffer.truncate(size);
            let data = mem::replace(&mut buffer, BytesMut::new());
            buffer.resize(65536, 0);
            let data = data.freeze();
            if let Err(_) = self.queue.send(UdpMessage { address, data }) {
                // Do some analytics maybe or smth
                println!("Dropped UDP packet");
                drop_counter += 1;
            }
        }
    }

    pub fn socket(&self) -> &UdpSocket {
        &self.socket
    }

    pub fn receiver(&self) -> Receiver<UdpMessage> {
        self.queue_receiver.clone()
    }
}
