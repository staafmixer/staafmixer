use crate::threaded_rtp_handler::INGEST_MANAGER;
use crate::{IncomingStreamHandle, SessionRef, Stream, StreamId};
use janus_plugin::sdp::Sdp;
use staaf_core::rpc::MediaStream;
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread::{spawn, JoinHandle};

pub struct Router {
    sessions: HashSet<Box<SessionRef>>,
    streams: HashMap<StreamId, Stream>,
    early_subscribers: HashMap<StreamId, Vec<SessionRef>>,
    stream_map: HashMap<IncomingStreamHandle, StreamId>,
}

static EMPTY_SUBSCRIBERS: Vec<SessionRef> = vec![];

static mut SDP_THREAD: Option<JoinHandle<()>> = None;
static mut SDP_RPC_SENDER: Option<Sender<SDPThreadRPC>> = None;

pub fn start_sdp_thread() {
    let (sender, receiver) = channel();

    unsafe {
        SDP_RPC_SENDER = Some(sender);
        SDP_THREAD = Some(spawn(|| sdp_thread(receiver)));
    }
}

pub fn queue_rpc(msg: SDPThreadRPC) {
    unsafe { SDP_RPC_SENDER.as_ref().expect("init() not run yet?") }
        .send(msg)
        .expect("Failed queueing message")
}

pub enum SDPThreadRPC {
    Offer(Sdp),
    Answer(Sdp),
}

fn sdp_thread(receiver: Receiver<SDPThreadRPC>) {
    for _rpc in receiver {}
}

impl Router {
    pub fn new() -> Router {
        Router {
            sessions: Default::default(),
            streams: Default::default(),
            early_subscribers: Default::default(),
            stream_map: Default::default(),
        }
    }

    pub fn disconnect(&mut self, session: SessionRef) {
        self.sessions.retain(|item| item.handle != session.handle);
        for stream_id in session.subscribed_to() {
            self.remove_route(&session, stream_id);
        }
    }

    pub fn connect(&mut self, session: Box<SessionRef>) {
        self.sessions.insert(session);
    }

    pub fn remove_route(&mut self, session: &SessionRef, stream_id: StreamId) {
        if let Some(stream) = self.streams.get_mut(&stream_id) {
            stream
                .subscribers
                .retain(|item| !Arc::ptr_eq(item, session));
        }

        if let Some(subscribers) = self.early_subscribers.get_mut(&stream_id) {
            subscribers.retain(|item| !Arc::ptr_eq(item, session))
        }
    }

    pub fn add_route(&mut self, session: &SessionRef, stream_id: StreamId) {
        println!(
            "Session#{} subscribed to {}",
            session.handle as usize, stream_id
        );
        session.subscribe(stream_id);
        if let Some(stream) = self.streams.get_mut(&stream_id) {
            stream.subscribers.push(Arc::clone(session));
            return;
        }

        self.early_subscribers
            .entry(stream_id)
            .or_default()
            .push(Arc::clone(session));
    }

    pub fn get_stream(&self, stream_index: StreamId) -> Option<&Stream> {
        self.streams.get(&stream_index)
    }

    pub fn get_stream_mut(&mut self, stream_index: StreamId) -> Option<&mut Stream> {
        self.streams.get_mut(&stream_index)
    }

    pub fn get_stream_id(&self, ssrc: u32, port: u16, ip_addr: IpAddr) -> Option<StreamId> {
        self.stream_map
            .get(&IncomingStreamHandle {
                ssrc,
                incoming_port: port,
                peer_addr: ip_addr,
            })
            .copied()
    }

    pub fn register_stream(
        &mut self,
        stream_index: StreamId,
        media_streams: Vec<MediaStream>,
        port: u16,
        ip_addr: IpAddr,
    ) {
        let mut stream = Stream::new(stream_index, media_streams.clone(), port, ip_addr);

        if let Some(subs) = self.early_subscribers.remove(&stream_index) {
            stream.subscribers = subs;
        }

        self.streams.insert(stream_index, stream);
        for media_stream in &media_streams {
            let handle = IncomingStreamHandle {
                ssrc: media_stream.ssrc,
                incoming_port: port,
                peer_addr: ip_addr,
            };
            self.stream_map.insert(handle, stream_index);
        }

        {
            let mut manager = INGEST_MANAGER.write().unwrap();
            manager.register_stream(stream_index, ip_addr, port, media_streams);
        }
    }

    pub fn _destroy_stream(&mut self, stream_index: StreamId) {
        if let Some(stream) = self.streams.remove(&stream_index) {
            for media_stream in stream.media_streams {
                self.stream_map.remove(&IncomingStreamHandle {
                    ssrc: media_stream.ssrc,
                    incoming_port: stream._port,
                    peer_addr: stream._peer_addr,
                });
            }
        }
    }

    pub fn get_subscribers(&self, stream_index: StreamId) -> &Vec<SessionRef> {
        if let Some(stream) = self.streams.get(&stream_index) {
            &stream.subscribers
        } else {
            &EMPTY_SUBSCRIBERS
        }
    }
}
