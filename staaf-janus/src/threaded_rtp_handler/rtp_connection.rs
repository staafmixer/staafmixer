use crate::threaded_rtp_handler::rtp_utils::{MuxedRtp, RtcpHeader, RtpHeader};
use crate::threaded_rtp_handler::{UdpMessage, UdpRtpMessage, RTP_MESSAGE_QUEUE_DEPTH};
use crate::{callback, router, router_mut};
use byteorder::{BigEndian, WriteBytesExt};
use bytes::{BufMut, Bytes, BytesMut};
use crossbeam_channel::{bounded, Receiver, Sender};
use janus_plugin_sys::plugin::{janus_plugin_rtcp, janus_plugin_rtp, janus_plugin_rtp_extensions};
use staaf_core::rpc::MediaStream;
use std::collections::HashSet;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::SystemTime;

pub struct RtpConnectionHandle {
    pub drop_counter: AtomicU64,
    pub queue: Sender<UdpRtpMessage>,
    pub stream_index: u32,
}

pub struct RtpConnection {
    handle: Arc<RtpConnectionHandle>,
    queue_receiver: Receiver<UdpRtpMessage>,
    udp_socket: UdpSocket,
    rtp_streams: Vec<RtpStream>,
    stream_index: u32,
}

pub struct RtpStream {
    ssrc: u32,
    stream_index: u32,
    media_stream: MediaStream,
    udp_socket: UdpSocket,
    tracker: RtpMessageTracker,
}

impl RtpStream {
    pub fn new(udp_socket: UdpSocket, media_stream: MediaStream, stream_index: u32) -> RtpStream {
        RtpStream {
            ssrc: media_stream.ssrc,
            stream_index,
            media_stream,
            udp_socket,
            tracker: RtpMessageTracker::new(0),
        }
    }

    pub fn handle_message(&mut self, message: UdpRtpMessage) {
        match message.rtp {
            MuxedRtp::RTP(rtp) => self.handle_rtp(rtp, message.data, message.address),
            MuxedRtp::RTCP(rtcp) => self.handle_rtcp(rtcp, message.data, message.address),
        }
    }

    pub fn handle_rtp(&mut self, rtp: RtpHeader, data: Bytes, addr: SocketAddr) {
        match self.tracker.track(rtp.seq_nr) {
            TrackerStatus::MissingPackets(drift) if drift > 15 => {
                println!("rtp stream missing packets, drift of {}", drift);
                self.send_nacks(addr);
            }
            TrackerStatus::OutOfSync => {
                println!("rtp stream out of sync, resetting");
                self.tracker.reset(rtp.seq_nr);
                // TODO: Send request for i-frame
            }

            // Ok & drift < 16
            _ => {}
        }

        let relay = callback().relay_rtp;

        let mut data = data.to_vec();

        let mut rtp_packet = Box::new(janus_plugin_rtp {
            video: if self.media_stream.content.is_video() {
                1
            } else {
                0
            },
            buffer: data.as_mut_ptr().cast(),
            length: data.len() as i16,
            extensions: janus_plugin_rtp_extensions {
                audio_level: 0,
                audio_level_vad: 0,
                video_rotation: 0,
                video_back_camera: 0,
                video_flipped: 0,
            },
        });

        for sub in router().get_subscribers(self.stream_index) {
            relay(sub.handle, rtp_packet.as_mut());
        }
    }

    pub fn handle_rtcp(&mut self, rtcp: RtcpHeader, data: Bytes, addr: SocketAddr) {
        if rtcp.payload_type == 200 {
            let mut header: u8 = 0;
            header |= 2 << 6;
            header |= 1;
            let epoch = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap();

            let mut packet = &mut [header, 250, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
            packet[4..8].copy_from_slice(&(epoch.as_secs() as u32).to_be_bytes());
            packet[8..12].copy_from_slice(&(epoch.subsec_nanos() as u32).to_be_bytes());
            self.udp_socket.send_to(packet, addr).unwrap();
            // Ignore this weird invalid RTCP
            return;
        }

        let relay = callback().relay_rtcp;

        let mut data = data.to_vec();
        let mut rtcp_packet = Box::new(janus_plugin_rtcp {
            video: if self.media_stream.content.is_video() {
                1
            } else {
                0
            },
            buffer: data.as_mut_ptr().cast(),
            length: data.len() as i16,
        });

        for sub in router().get_subscribers(self.stream_index) {
            relay(sub.handle, rtcp_packet.as_mut());
        }
    }

    pub fn send_nacks(&mut self, addr: SocketAddr) {
        let mut seqs = self.tracker.get_and_mark_nacks();
        if seqs.is_empty() {
            return;
        }

        let mut packet = BytesMut::new();
        let mut writer = packet.writer();
        let mut header: u8 = 0;
        // Version field always 2
        header |= 2 << 6;
        // FB message type 1 = Generic NACK
        header |= 1;
        writer.write_u8(header);
        // Payload type 205 = RTPFB / Transport layer FB message
        writer.write_u8(205);
        // Length field, will be set later;
        writer.write_u16::<BigEndian>(0);
        // our SSRC, can be ignored
        writer.write_u32::<BigEndian>(0);
        // target SSRC
        writer.write_u32::<BigEndian>(self.ssrc);

        let mut current = seqs.remove(0);
        let mut bitfield = 1;
        let mut items: u16 = 1;

        for seq in seqs {
            let shift = wrapping_difference(current, seq);
            if shift > 15 {
                writer.write_u16::<BigEndian>(current);
                writer.write_u16::<BigEndian>(bitfield);
                current = seq;
                bitfield = 1;
                items += 1;
                continue;
            }

            bitfield |= 1 >> shift;
        }

        writer.write_u16::<BigEndian>(current);
        writer.write_u16::<BigEndian>(bitfield);
        let mut packet = writer.into_inner();
        packet[2..4].copy_from_slice(&(2u16 + items).to_be_bytes());
        self.udp_socket
            .send_to(&packet, addr)
            .expect("Failed to send generic NACKS package");
    }
}

impl RtpConnection {
    pub fn new(
        udp_socket: UdpSocket,
        media_streams: Vec<MediaStream>,
        stream_index: u32,
    ) -> RtpConnection {
        let (sender, receiver) = bounded(RTP_MESSAGE_QUEUE_DEPTH);

        let handle = RtpConnectionHandle {
            drop_counter: AtomicU64::new(0),
            queue: sender,
            stream_index,
        };

        let mut rtp_streams = vec![];
        for media_stream in media_streams {
            let socket = udp_socket.try_clone().expect("Failed to clone UDP socket");
            rtp_streams.push(RtpStream::new(socket, media_stream, stream_index));
        }

        RtpConnection {
            handle: Arc::new(handle),
            queue_receiver: receiver,
            udp_socket,
            rtp_streams,
            stream_index,
        }
    }

    pub fn handle(&self) -> Arc<RtpConnectionHandle> {
        Arc::clone(&self.handle)
    }

    pub fn run(&mut self) {
        while let Ok(message) = self.queue_receiver.recv() {
            self.handle_message(message);
        }
    }

    fn handle_message(&mut self, message: UdpRtpMessage) {
        let ssrc = message.rtp.ssrc();
        for stream in &mut self.rtp_streams {
            if stream.ssrc == ssrc {
                stream.handle_message(message);
                break;
            }
        }
    }
}

#[derive(Debug)]
struct RtpMessageTracker {
    nacks_sent: HashSet<u16>,
    last_in_sequence: u16,
    latest: u16,
    seen: HashSet<u16>,
}

fn wrapping_difference(lhs: u16, rhs: u16) -> u16 {
    (u16::MAX - lhs).wrapping_add(rhs).wrapping_add(1)
}

pub enum TrackerStatus {
    Ok,
    MissingPackets(u16),
    OutOfSync,
}

impl RtpMessageTracker {
    pub fn new(seq: u16) -> RtpMessageTracker {
        RtpMessageTracker {
            nacks_sent: Default::default(),
            last_in_sequence: seq,
            latest: seq,
            seen: Default::default(),
        }
    }

    pub fn track(&mut self, seq: u16) -> TrackerStatus {
        let change = wrapping_difference(self.last_in_sequence, seq);
        if change == 0 {
            // resent?
            return TrackerStatus::Ok;
        }

        if wrapping_difference(self.latest, seq) < 100 {
            self.latest = seq;
        }

        self.nacks_sent.remove(&seq);

        if change == 1 {
            self.last_in_sequence = seq;
        } else {
            self.seen.insert(seq);
            let diff = wrapping_difference(self.last_in_sequence, self.latest);

            if diff > 100 {
                return TrackerStatus::OutOfSync;
            }

            return TrackerStatus::MissingPackets(diff);
        }

        let mut next = self.last_in_sequence.wrapping_add(1);
        while self.seen.remove(&next) {
            self.last_in_sequence = next;
            next = next.wrapping_add(1)
        }

        if self.last_in_sequence == self.latest {
            return TrackerStatus::Ok;
        }

        let diff = wrapping_difference(self.last_in_sequence, self.latest);
        if diff > 100 {
            return TrackerStatus::OutOfSync;
        }

        TrackerStatus::MissingPackets(diff)
    }

    pub fn reset(&mut self, seq: u16) {
        self.last_in_sequence = seq;
        self.latest = seq;
        self.seen.clear();
        self.nacks_sent.clear();
    }

    pub fn get_and_mark_nacks(&mut self) -> Vec<u16> {
        let mut missing = vec![];
        let mut seq = self.last_in_sequence.wrapping_add(1);
        while seq != self.latest {
            if !self.nacks_sent.contains(&seq) && !self.seen.contains(&seq) {
                missing.push(seq);
                self.nacks_sent.insert(seq);
            }

            seq = seq.wrapping_add(1)
        }

        missing
    }
}
