mod rpc;

use crate::rpc::{MediaStream, StaafRPC};
use janus_plugin::sdp::Sdp;
use janus_plugin::*;
use serde::de::DeserializeOwned;
use std::collections::{HashMap, HashSet};
use std::convert::TryInto;
use std::ffi::CStr;
use std::net::{IpAddr, UdpSocket};
use std::option::Option::Some;
use std::os::raw::{c_char, c_int};
use std::ptr::null_mut;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

static mut CALLBACKS: Option<&PluginCallbacks> = None;
static mut ROUTER: Option<Arc<RwLock<Router>>> = None;

type StreamId = u32;

#[derive(Debug, Copy, Clone, Hash, Ord, PartialOrd, Eq, PartialEq)]
struct IncomingStreamHandle {
    ssrc: u32,
    incoming_port: u16,
    peer_addr: IpAddr,
}

struct Stream {
    id: StreamId,
    media_streams: Vec<MediaStream>,
    port: u16,
    peer_addr: IpAddr,
    subscribers: Vec<SessionRef>,
}

impl Stream {
    fn new(id: StreamId, media_streams: Vec<MediaStream>, port: u16, peer_addr: IpAddr) -> Stream {
        Stream {
            id,
            media_streams,
            port,
            peer_addr,
            subscribers: Default::default(),
        }
    }
}

struct SessionState {
    subscribed_to: RwLock<HashSet<StreamId>>,
}

impl SessionState {
    fn subscribed_to(&self) -> Vec<StreamId> {
        self.subscribed_to
            .read()
            .expect("Session poisoned")
            .iter()
            .copied()
            .collect()
    }

    fn subscribe(&self, stream_index: StreamId) {
        self.subscribed_to
            .write()
            .expect("Session poisoned")
            .insert(stream_index);
    }
}

struct Router {
    sessions: HashSet<Box<SessionRef>>,
    streams: HashMap<StreamId, Stream>,
    stream_map: HashMap<IncomingStreamHandle, StreamId>,
}

static EMPTY_SUBSCRIBERS: Vec<SessionRef> = vec![];

impl Router {
    fn new() -> Router {
        Router {
            sessions: Default::default(),
            streams: Default::default(),
            stream_map: Default::default(),
        }
    }

    pub fn disconnect(&mut self, session: SessionRef) {
        self.sessions.retain(|item| item.handle != session.handle);
        for stream_id in session.subscribed_to() {
            self.remove_route(&session, stream_id);
        }
    }

    fn connect(&mut self, session: Box<SessionRef>) {
        self.sessions.insert(session);
    }

    fn remove_route(&mut self, session: &SessionRef, stream_id: StreamId) {
        let stream = if let Some(stream) = self.streams.get_mut(&stream_id) {
            stream
        } else {
            return;
        };

        stream.subscribers.retain(|item| Arc::ptr_eq(item, session));
    }

    fn add_route(&mut self, session: &SessionRef, stream_id: StreamId) {
        session.subscribe(stream_id);
        let stream = if let Some(stream) = self.streams.get_mut(&stream_id) {
            stream
        } else {
            return;
        };

        stream.subscribers.push(Arc::clone(session));
    }

    fn get_stream(&self, stream_index: StreamId) -> Option<&Stream> {
        self.streams.get(&stream_index)
    }

    fn get_stream_id(&self, ssrc: u32, port: u16, ip_addr: IpAddr) -> Option<StreamId> {
        self.stream_map
            .get(&IncomingStreamHandle {
                ssrc,
                incoming_port: port,
                peer_addr: ip_addr,
            })
            .copied()
    }

    fn register_stream(
        &mut self,
        stream_index: StreamId,
        media_streams: Vec<MediaStream>,
        port: u16,
        ip_addr: IpAddr,
    ) {
        let stream = Stream::new(stream_index, media_streams.clone(), port, ip_addr);
        self.streams.insert(stream_index, stream);
        for media_stream in media_streams {
            self.stream_map.insert(
                IncomingStreamHandle {
                    ssrc: media_stream.ssrc,
                    incoming_port: port,
                    peer_addr: ip_addr,
                },
                stream_index,
            );
        }
    }

    fn destroy_stream(&mut self, stream_index: StreamId) {
        if let Some(stream) = self.streams.remove(&stream_index) {
            for media_stream in stream.media_streams {
                self.stream_map.remove(&IncomingStreamHandle {
                    ssrc: media_stream.ssrc,
                    incoming_port: stream.port,
                    peer_addr: stream.peer_addr,
                });
            }
        }
    }

    fn get_subscribers(&self, stream_index: StreamId) -> &Vec<SessionRef> {
        if let Some(stream) = self.streams.get(&stream_index) {
            &stream.subscribers
        } else {
            &EMPTY_SUBSCRIBERS
        }
    }
}

macro_rules! or_continue {
    ($data:expr) => {
        if let Some(data) = $data {
            data
        } else {
            continue;
        }
    };
}

macro_rules! or_return {
    ($data:expr) => {
        if let Some(data) = $data {
            data
        } else {
            return;
        }
    };

    ($data:expr, $ret:expr) => {
        if let Some(data) = $data {
            data
        } else {
            return $ret;
        }
    };
}

impl SessionState {
    fn new() -> SessionState {
        SessionState {
            subscribed_to: Default::default(),
        }
    }
}

type Session = SessionWrapper<SessionState>;
type SessionRef = Arc<Session>;

struct RTPReceiver {
    port: u16,
    udp_socket: UdpSocket,
}

impl RTPReceiver {
    fn new() -> RTPReceiver {
        let socket = UdpSocket::bind("0.0.0.0:1445").unwrap();
        RTPReceiver {
            port: socket.peer_addr().unwrap().port(),
            udp_socket: socket,
        }
    }

    fn start(self) {
        std::thread::spawn(move || {
            self.receive();
        });
    }

    fn receive(&self) {
        let relay = callback().relay_rtp;
        let mut buffer = [0u8; 4096];
        while let Ok((len, addr)) = self.udp_socket.recv_from(&mut buffer) {
            if len < 12 {
                // wtf?
                continue;
            }

            let ssrc = u32::from_be_bytes(buffer[8..12].try_into().unwrap());
            let stream_id = or_continue!(router().get_stream_id(ssrc, self.port, addr.ip()));

            let rtp = PluginRtpPacket {
                video: if ssrc == stream_id { 1 } else { 0 },
                buffer: buffer.as_mut_ptr().cast(),
                length: len as i16,
                extensions: janus_plugin::PluginRtpExtensions {
                    audio_level: 0,
                    audio_level_vad: 0,
                    video_rotation: 0,
                    video_back_camera: 0,
                    video_flipped: 0,
                },
            };

            let mut boxed = Box::new(rtp);

            for subscriber in router().get_subscribers(stream_id) {
                relay(subscriber.handle, boxed.as_mut());
            }
        }
    }
}

// courtesy of c_string crate, which also has some other stuff we aren't interested in
// taking in as a dependency here.
macro_rules! c_str {
    ($lit:expr) => {
        unsafe { CStr::from_ptr(concat!($lit, "\0").as_ptr() as *const $crate::c_char) }
    };
}

fn parse_json_raw<T: DeserializeOwned>(raw: *mut RawJanssonValue) -> Option<T> {
    let value = unsafe { JanssonValue::from_raw(raw) }?;
    let json_libc = value.to_libcstring(JanssonEncodingFlags::empty());
    let json = json_libc.to_str().ok()?;
    serde_json::from_str::<T>(json).ok()
}

fn callback() -> &'static PluginCallbacks {
    unsafe { CALLBACKS.expect("Failed to get callbacks, has init() not run yet?") }
}

fn router() -> RwLockReadGuard<'static, Router> {
    unsafe {
        ROUTER
            .as_ref()
            .expect("Failed to get router, has init() not run yet?")
            .read()
            .expect("Router poisoned")
    }
}

fn router_mut() -> RwLockWriteGuard<'static, Router> {
    unsafe {
        ROUTER
            .as_ref()
            .expect("Failed to get router, has init() not run yet?")
            .write()
            .expect("Router poisoned")
    }
}

extern "C" fn init(callbacks: *mut PluginCallbacks, _config_path: *const c_char) -> c_int {
    janus_info!("Staafmixer is alive!");

    unsafe {
        ROUTER = Some(Arc::new(RwLock::new(Router::new())));
    }

    match unsafe { callbacks.as_ref() } {
        Some(c) => unsafe { CALLBACKS = Some(c) },

        None => {
            janus_err!("Staafmixer didn't get any callbacks! I am quitting!");
            return -1;
        }
    }

    RTPReceiver::new().start();
    0
}

extern "C" fn destroy() {
    janus_info!("Staafmixer is dead! :D oh :(");
}

extern "C" fn create_session(handle: *mut PluginSession, error: *mut c_int) {
    let state = SessionState::new();
    if let Ok(session) = unsafe { Session::associate(handle, state) } {
        router_mut().connect(session);
    } else {
        janus_err!("Failed to associate session!");
        unsafe {
            *error = -1;
        }
    }
}

extern "C" fn query_session(_handle: *mut PluginSession) -> *mut RawJanssonValue {
    std::ptr::null_mut()
}

extern "C" fn destroy_session(handle: *mut PluginSession, error: *mut c_int) {
    match unsafe { Session::from_ptr(handle) } {
        Ok(session) => {
            router_mut().disconnect(session);
        }

        Err(err) => {
            janus_err!("{}", err);
            unsafe { *error = -1 }
        }
    }
}

pub fn get_free_port(_media_streams: &Vec<MediaStream>) -> u16 {
    1445
}

extern "C" fn handle_message(
    handle: *mut PluginSession,
    _transaction: *mut c_char,
    message: *mut RawJanssonValue,
    jsep: *mut RawJanssonValue,
) -> *mut RawPluginResult {
    let msg: StaafRPC = or_return!(
        parse_json_raw(message),
        PluginResult::error(c_str!("Message not found")).into_raw()
    );
    match msg {
        StaafRPC::RegisterStream(reg) => {
            let new_port = get_free_port(&reg.media_streams);
            router_mut().register_stream(reg.stream_id, reg.media_streams, new_port, reg.peer_addr);

            PluginResult::ok(
                JanssonValue::from_str(
                    &format!("{{\"port\": {}}}", new_port),
                    JanssonDecodingFlags::empty(),
                )
                .unwrap(),
            )
            .into_raw()
        }
        StaafRPC::Subscribe(stream_id) => {
            let wrapper = or_return!(
                Session::from_ptr(handle).ok(),
                PluginResult::error(c_str!("Session not found")).into_raw()
            );

            router_mut().add_route(&wrapper, stream_id);

            PluginResult::ok(JanssonValue::from_str("{}", JanssonDecodingFlags::empty()).unwrap())
                .into_raw()
        }
        StaafRPC::Unsubscribe(stream_id) => {
            let wrapper = or_return!(
                Session::from_ptr(handle).ok(),
                PluginResult::error(c_str!("Session not found")).into_raw()
            );

            router_mut().remove_route(&wrapper, stream_id);

            PluginResult::ok(JanssonValue::from_str("{}", JanssonDecodingFlags::empty()).unwrap())
                .into_raw()
        }
        StaafRPC::Offer => {
            let push = callback().push_event;

            // TODO: Handle offer and answer

            PluginResult::ok_wait(Some(c_str!("Processing.")))
        }
    }
}

extern "C" fn setup_media(_handle: *mut PluginSession) {}

extern "C" fn hangup_media(_handle: *mut PluginSession) {}

extern "C" fn incoming_rtp(
    _handle: *mut PluginSession,
    _packet: *mut janus_plugin::PluginRtpPacket,
) {
}

extern "C" fn incoming_rtcp(
    _handle: *mut PluginSession,
    _packet: *mut janus_plugin::PluginRtcpPacket,
) {
}

unsafe extern "C" fn handle_admin_message(_message: *mut RawJanssonValue) -> *mut RawJanssonValue {
    null_mut()
}

extern "C" fn incoming_data(
    _handle: *mut PluginSession,
    _packet: *mut janus_plugin::PluginDataPacket,
) {
}

extern "C" fn slow_link(_handle: *mut PluginSession, _uplink: c_int, _video: c_int) {}

const PLUGIN: Plugin = build_plugin!(
    LibraryMetadata {
        api_version: 15,
        version: 1,
        name: c_str!("Staafmixer Janus Plugin"),
        package: c_str!("eater.staafmixer"),
        version_str: c_str!(env!("CARGO_PKG_VERSION")),
        description: c_str!(env!("CARGO_PKG_DESCRIPTION")),
        author: c_str!(env!("CARGO_PKG_AUTHORS")),
    },
    init,
    destroy,
    create_session,
    handle_message,
    setup_media,
    incoming_rtp,
    incoming_rtcp,
    incoming_data,
    slow_link,
    hangup_media,
    destroy_session,
    query_session,
    handle_admin_message
);

export_plugin!(&PLUGIN);
