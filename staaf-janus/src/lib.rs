#![allow(unused_unsafe)]

mod router;
mod rtp_receiver;

use crate::router::Router;
use crate::rtp_receiver::RTPReceiver;
use discortp::rtcp::report::{ReceiverReportPacket, ReportBlockIterable, ReportBlockPacket};
use discortp::rtcp::RtcpType::ReceiverReport;
use discortp::FromPacket;
use glib_sys::g_list_append;
use janus_plugin::sdp::Sdp;
use janus_plugin::*;
use janus_plugin_sys::rtcp::janus_rtcp_get_remb;
use janus_plugin_sys::sdp::janus_sdp_mdirection::JANUS_SDP_SENDONLY;
use janus_plugin_sys::sdp::janus_sdp_mtype::{JANUS_SDP_AUDIO, JANUS_SDP_VIDEO};
use janus_plugin_sys::sdp::{
    janus_sdp_attribute_add_to_mline, janus_sdp_attribute_create, janus_sdp_mline_create,
    janus_sdp_new,
};
use serde::de::DeserializeOwned;
use serde::export::fmt::Debug;
use serde::{Deserialize, Serialize};
use serde_json::json;
use staaf_core::rpc::{
    AudioCodec, MediaStream, MediaStreamAudio, MediaStreamContent, MediaStreamVideo, StaafRPC,
    VideoCodec,
};
use staaf_core::StreamId;
use std::collections::HashSet;
use std::ffi::{CStr, CString};
use std::mem;
use std::net::{IpAddr, Ipv4Addr};
use std::option::Option::Some;
use std::os::raw::{c_char, c_int, c_void};
use std::ptr::{null, null_mut};
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

static mut CALLBACKS: Option<&PluginCallbacks> = None;
static mut ROUTER: Option<Arc<RwLock<Router>>> = None;

#[derive(Debug, Copy, Clone, Hash, Ord, PartialOrd, Eq, PartialEq)]
pub struct IncomingStreamHandle {
    ssrc: u32,
    incoming_port: u16,
    peer_addr: IpAddr,
}

pub struct Stream {
    _id: StreamId,
    media_streams: Vec<MediaStream>,
    _port: u16,
    _peer_addr: IpAddr,
    subscribers: Vec<SessionRef>,
}

impl Stream {
    fn new(id: StreamId, media_streams: Vec<MediaStream>, port: u16, peer_addr: IpAddr) -> Stream {
        Stream {
            _id: id,
            media_streams,
            _port: port,
            _peer_addr: peer_addr,
            subscribers: Default::default(),
        }
    }
}

pub struct SessionState {
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

#[macro_export]
macro_rules! or_continue {
    ($data:expr) => {
        if let Some(data) = $data {
            data
        } else {
            continue;
        }
    };
}

#[macro_export]
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

#[derive(Deserialize, Serialize, Debug)]
#[serde(tag = "type", content = "sdp", rename_all = "lowercase")]
enum JsepObject {
    Offer(Sdp),
    Answer(Sdp),
}

type Session = SessionWrapper<SessionState>;
type SessionRef = Arc<Session>;

// courtesy of c_string crate, which also has some other stuff we aren't interested in
// taking in as a dependency here.
macro_rules! c_str {
    ($lit:expr) => {
        unsafe { CStr::from_ptr(concat!($lit, "\0").as_ptr() as *const $crate::c_char) }
    };
}

trait SerializeJansson {
    fn serialize_jansson(&self) -> JanssonValue {
        return JanssonValue::from_str(&self.serialize_json(), JanssonDecodingFlags::empty())
            .unwrap();
    }

    fn serialize_jansson_raw(&self) -> *mut RawJanssonValue {
        self.serialize_jansson().into_raw()
    }

    fn serialize_json(&self) -> String;
}

impl<T: Serialize> SerializeJansson for T {
    fn serialize_json(&self) -> String {
        serde_json::to_string(&self).unwrap()
    }
}

fn parse_json_raw<T: DeserializeOwned + Debug>(raw: *mut RawJanssonValue) -> Option<T> {
    println!("JSON is null: {}", raw.is_null());
    let value = unsafe { JanssonValue::from_raw(raw) }?;
    let json_libc = value.to_libcstring(JanssonEncodingFlags::empty());
    let json = json_libc.to_str().ok()?;
    println!("Parsing: {}", json);
    let res = serde_json::from_str::<T>(json);
    println!("Parsed: {:?}", res);
    res.ok()
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

    router_mut().register_stream(
        1337,
        vec![
            MediaStream {
                ssrc: 1338,
                payload_type: 96,
                content: MediaStreamContent::Video(MediaStreamVideo {
                    codec: VideoCodec::H264,
                    width: 1280,
                    height: 720,
                }),
            },
            MediaStream {
                ssrc: 1337,
                payload_type: 97,
                content: MediaStreamContent::Audio(MediaStreamAudio {
                    codec: AudioCodec::Opus,
                }),
            },
        ],
        40,
        IpAddr::V4(Ipv4Addr::LOCALHOST),
    );

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
    println!("Staaf is destroying a session");
    match unsafe { Session::from_ptr(handle) } {
        Ok(session) => {
            println!("Disconnecting session: {}", unsafe {
                (*session.handle).plugin_handle as u64
            });
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
    transaction: *mut c_char,
    message: *mut RawJanssonValue,
    _jsep: *mut RawJanssonValue,
) -> *mut RawPluginResult {
    let msg: StaafRPC = or_return!(
        parse_json_raw(message),
        PluginResult::error(c_str!("Message not found")).into_raw()
    );

    println!("Message: {:?}", msg);

    match msg {
        StaafRPC::RegisterStream(reg) => {
            let new_port = get_free_port(&reg.media_streams);
            router_mut().register_stream(reg.stream_id, reg.media_streams, new_port, reg.peer_addr);

            PluginResult::ok(
                JanssonValue::from_str(
                    serde_json::to_string(&StaafRPC::StreamRegistered { port: new_port })
                        .unwrap()
                        .as_str(),
                    JanssonDecodingFlags::empty(),
                )
                .unwrap(),
            )
            .into_raw()
        }
        StaafRPC::Subscribe { stream_id } => {
            let wrapper = or_return!(
                unsafe { Session::from_ptr(handle) }.ok(),
                PluginResult::error(c_str!("Session not found")).into_raw()
            );

            if wrapper
                .subscribed_to
                .read()
                .expect("Session poisoned")
                .len()
                > 0
            {
                return PluginResult::error(c_str!("No multistream support yet")).into_raw();
            }

            router_mut().add_route(&wrapper, stream_id);

            PluginResult::ok(JanssonValue::from_str("{}", JanssonDecodingFlags::empty()).unwrap())
                .into_raw()
        }
        StaafRPC::Unsubscribe { stream_id } => {
            let wrapper = or_return!(
                unsafe { Session::from_ptr(handle) }.ok(),
                PluginResult::error(c_str!("Session not found")).into_raw()
            );

            router_mut().remove_route(&wrapper, stream_id);

            PluginResult::ok(JanssonValue::from_str("{}", JanssonDecodingFlags::empty()).unwrap())
                .into_raw()
        }
        StaafRPC::Offer => {
            let wrapper = or_return!(
                unsafe { Session::from_ptr(handle) }.ok(),
                PluginResult::error(c_str!("Session not found")).into_raw()
            );

            let subscribe_to = wrapper.subscribed_to.read().expect("Session poisoned");
            if subscribe_to.len() == 0 {
                return PluginResult::ok(
                    json!({
                        "status": "empty"
                    })
                    .serialize_jansson(),
                )
                .into_raw();
            }

            println!("Creating answer");

            let media_streams = subscribe_to
                .iter()
                .filter_map(|channel_id| {
                    router()
                        .get_stream(*channel_id)
                        .map(|stream| stream.media_streams.clone())
                })
                .flat_map(|list| list)
                .collect::<Vec<_>>();

            let offer = unsafe {
                let sdp = janus_sdp_new(null(), c_str!("127.0.0.1").as_ptr());

                for media_stream in media_streams {
                    let mline = janus_sdp_mline_create(
                        if let MediaStreamContent::Video(_) = &media_stream.content {
                            JANUS_SDP_VIDEO
                        } else {
                            JANUS_SDP_AUDIO
                        },
                        1,
                        c_str!("UDP/TLS/RTP/SAVPF").as_ptr(),
                        JANUS_SDP_SENDONLY,
                    );

                    (*mline).ptypes = g_list_append(
                        (*mline).ptypes,
                        null_mut::<c_void>().offset(media_stream.payload_type as isize),
                    );

                    let codec_info = match &media_stream.content {
                        MediaStreamContent::Audio(audio) => {
                            format!("{}/48000/2", audio.codec.to_str())
                        }
                        MediaStreamContent::Video(video) => {
                            format!("{}/90000", video.codec.to_str())
                        }
                    };
                    let c_str =
                        CString::new(format!("{} {}", media_stream.payload_type, codec_info))
                            .unwrap();
                    let rtpmap =
                        janus_sdp_attribute_create(c_str!("rtpmap").as_ptr(), c_str.as_ptr());
                    janus_sdp_attribute_add_to_mline(mline, rtpmap);

                    match &media_stream.content {
                        MediaStreamContent::Video(_video) => {
                            let c_str = CString::new(format!(
                                "{} profile-level-id=42e01f;level-asymmetry-allowed=1",
                                media_stream.payload_type
                            ))
                            .unwrap();
                            let fmtp =
                                janus_sdp_attribute_create(c_str!("fmtp").as_ptr(), c_str.as_ptr());
                            janus_sdp_attribute_add_to_mline(mline, fmtp);
                        }
                        _ => {}
                    }

                    (*sdp).m_lines = g_list_append((*sdp).m_lines, mline.cast());
                }

                Sdp::new(sdp).unwrap()
            };

            (callback().push_event)(
                handle,
                &mut PLUGIN,
                transaction,
                json!({ "status": "offer" }).serialize_jansson_raw(),
                json!({ "type": "offer", "sdp": offer}).serialize_jansson_raw(),
            );

            println!("Generated SDP offer: {:?}", offer);

            PluginResult::ok_wait(None).into_raw()
        }
        StaafRPC::Answer => {
            return PluginResult::ok(json!({}).serialize_jansson()).into_raw();
        }

        _ => PluginResult::error(c_str!("Answer given as request")).into_raw(),
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
    handle: *mut PluginSession,
    packet: *mut janus_plugin::PluginRtcpPacket,
) {
    let xx = unsafe {
        Vec::from_raw_parts(
            (*packet).buffer.cast::<u8>(),
            (*packet).length as usize,
            (*packet).length as usize,
        )
    };
    let pkt = xx.clone();
    mem::forget(xx);

    println!(
        "{} => {:?}",
        handle as i64,
        ReceiverReportPacket::owned(pkt)
            .map(|x| x.from_packet())
            .and_then(|x| ReportBlockPacket::owned(x.payload))
            .map(|x| x.from_packet())
    );
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
