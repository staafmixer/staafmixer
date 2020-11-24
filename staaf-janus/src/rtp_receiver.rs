use crate::{callback, or_continue, or_return, router, router_mut};
use janus_plugin::PluginRtpPacket;
use janus_plugin_sys::plugin::{janus_plugin_rtcp, janus_plugin_rtp, janus_plugin_rtp_extensions};
use staaf_core::rpc::{MediaStream, MediaStreamContent, VideoCodec};
use std::collections::{HashMap, HashSet, VecDeque};
use std::convert::TryInto;
use std::net::{SocketAddr, UdpSocket};
use std::num::Wrapping;
use std::ops::Add;
use std::ptr::slice_from_raw_parts;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SEQUENCE_HISTORY: usize = 16;

#[derive(Debug, Clone)]
pub struct RTPStream {
    sequence: Option<Wrapping<u16>>,
    last_sequences: HashSet<u16>,
    last_in_sequence: Wrapping<u16>,
    nack_sent: HashSet<u16>,
    addr: SocketAddr,
    stream_start: Instant,
    last_sender_report: Option<Instant>,
    media_stream: MediaStream,
    keyframe_buffer: Vec<Vec<u8>>,
}

impl RTPStream {
    fn new(addr: SocketAddr, media_stream: MediaStream) -> RTPStream {
        RTPStream {
            sequence: None,
            last_sequences: Default::default(),
            last_in_sequence: Wrapping(0),
            nack_sent: Default::default(),
            addr,
            stream_start: Instant::now(),
            last_sender_report: None,
            media_stream,
            keyframe_buffer: vec![],
        }
    }

    pub fn push_sequence(&mut self, seq: Wrapping<u16>) -> Vec<(u16, u16)> {
        if self.sequence.is_none() {
            println!(
                "Stream[ssrc={}] starts with {:?}",
                self.media_stream.ssrc, seq
            );
            self.sequence = Some(seq);
            self.last_in_sequence = seq;
            return vec![];
        } else {
            // If missed more than 100 packets, unceremoniously continue as if
            if (seq - self.last_in_sequence).0 > 100 {
                println!("Stream[ssrc={}] reset to {:?}", self.media_stream.ssrc, seq);

                self.last_in_sequence = seq;
                self.sequence = Some(seq);
                self.last_sequences.clear();
                self.nack_sent.clear();
                return vec![];
            }
        }

        self.sequence = Some(seq);
        self.nack_sent.remove(&seq.0);

        if self.last_in_sequence + Wrapping(1u16) == seq {
            self.last_in_sequence = seq;
        } else {
            self.last_sequences.insert(seq.0);
        }

        while self
            .last_sequences
            .remove(&(self.last_in_sequence + Wrapping(1u16)).0)
        {
            self.last_in_sequence += Wrapping(1u16);
        }

        if self.last_sequences.len() > SEQUENCE_HISTORY {
            let mut missed = vec![];
            let mut done = 0;
            let mut offset = 1;
            let mut current: Option<Wrapping<u16>> = None;
            let mut bitfield: u16 = 0;
            println!("{:?} - {:?}", self.last_in_sequence, self.last_sequences);
            while done < self.last_sequences.len() {
                let item: Wrapping<u16> = self.last_in_sequence + Wrapping(offset);
                offset += 1;
                if self.last_sequences.contains(&item.0) {
                    done += 1;
                    continue;
                }

                if self.nack_sent.contains(&item.0) {
                    continue;
                }

                if let Some(current_wr) = current {
                    println!("{:?} {:?} {:?}", item, current_wr, (item - current_wr));
                    if (item - current_wr).0 > 15 {
                        missed.push((current_wr.0, bitfield));
                        current = None;
                    } else {
                        self.nack_sent.insert(item.0);
                        bitfield |= 1 << ((item - current_wr).0 - 1).max(0)
                    }
                }

                if current.is_none() {
                    current = Some(item);
                    bitfield = 1;
                    self.nack_sent.insert(item.0);
                }
            }

            if let Some(current_wr) = current {
                missed.push((current_wr.0, bitfield));
            }
            return missed;
        }

        vec![]
    }
}

pub struct RTPReceiver {
    port: u16,
    udp_socket: UdpSocket,
    rtp_status: HashMap<(u32, u32), RTPStream>,
}

pub struct RTPHeader {
    payload_type: u8,
    seq_nr: Wrapping<u16>,
    ssrc: u32,
    csrc_count: u8,
    extension: Option<RTPExtension>,
}

pub struct RTPExtension {
    id: u16,
    length: u16,
}

impl RTPHeader {
    fn parse(input: &[u8]) -> Option<RTPHeader> {
        if input.len() < 12 {
            return None;
        }

        let csrc_count = input[0] & 0b00001111;

        let extension = if (input[0] & 0b0001000) > 0 {
            let offset: usize = 12 + (csrc_count * 4) as usize;

            if (offset + 4) > input.len() {
                return None;
            }

            Some(RTPExtension {
                id: u16::from_be_bytes(input[offset..offset + 2].try_into().unwrap()),
                length: u16::from_be_bytes(input[offset + 2..offset + 4].try_into().unwrap()),
            })
        } else {
            None
        };

        Some(RTPHeader {
            payload_type: input[1],
            seq_nr: Wrapping(u16::from_be_bytes(input[2..4].try_into().unwrap())),
            ssrc: u32::from_be_bytes(input[8..12].try_into().unwrap()),
            csrc_count,
            extension,
        })
    }
}

pub struct RTCPHeader {
    payload_type: u8,
    ssrc: u32,
}

impl RTCPHeader {
    fn parse(input: &[u8]) -> Option<RTCPHeader> {
        if input.len() < 8 {
            return None;
        }

        Some(RTCPHeader {
            payload_type: input[1],
            ssrc: u32::from_be_bytes(input[4..8].try_into().unwrap()),
        })
    }
}

enum MuxedRTP {
    RTP(RTPHeader),
    RTCP(RTCPHeader),
}

impl MuxedRTP {
    fn ssrc(&self) -> u32 {
        match self {
            MuxedRTP::RTCP(rtcp) => rtcp.ssrc,
            MuxedRTP::RTP(rtp) => rtp.ssrc,
        }
    }

    fn seq_nr(&self) -> Wrapping<u16> {
        match self {
            MuxedRTP::RTCP(rtcp) => Wrapping(0),
            MuxedRTP::RTP(rtp) => rtp.seq_nr,
        }
    }

    fn parse(input: &[u8]) -> Option<MuxedRTP> {
        if input.len() < 8 {
            return None;
        }

        match input[1] {
            194 | 195 | 200..=213 => RTCPHeader::parse(input).map(MuxedRTP::RTCP),
            _ => RTPHeader::parse(input).map(MuxedRTP::RTP),
        }
    }
}

pub fn get_rtp_payload<'a>(input: &'a [u8]) -> Option<&'a [u8]> {
    let input = input.as_ref();
    if input.len() < 12 {
        return None;
    }

    let mut header_length: usize = 12;
    let header = RTPHeader::parse(input)?;
    header_length += header.csrc_count as usize * 4;

    if let Some(ext) = header.extension.as_ref() {
        header_length += 4 + ext.length as usize;
    }

    return Some(&input[header_length..]);
}

impl RTPReceiver {
    pub fn new() -> RTPReceiver {
        let socket = UdpSocket::bind("[::]:1445").unwrap();
        RTPReceiver {
            port: socket.local_addr().unwrap().port(),
            udp_socket: socket,
            rtp_status: Default::default(),
        }
    }

    pub fn start(mut self) {
        println!("Starting RTPReceiver");
        std::thread::spawn(move || {
            self.receive();
        });
    }

    fn receive(&mut self) {
        let mut buffer = [0u8; 4096];
        println!("Starting to receive udp");
        let mut c = 0;
        let mut start = Instant::now();
        while let Ok((len, addr)) = self.udp_socket.recv_from(&mut buffer) {
            if c == 100 {
                c = 0;
                let stop = Instant::now();
                let dur = stop.duration_since(start);
                println!(
                    "100 messages in {:?} ({}ns per message, {} msgs/sec)",
                    dur,
                    dur.as_nanos() / 100,
                    Duration::new(1, 0).as_nanos() / (dur.as_nanos() / 100)
                );

                start = Instant::now();
            }
            c += 1;

            let muxed = MuxedRTP::parse(&buffer[..len]);

            let muxed = if let Some(muxed) = muxed {
                muxed
            } else {
                continue;
            };

            // Get time as soon as possible before waiting on any locks
            let time = Instant::now();

            let ssrc = muxed.ssrc();
            // let stream_id = or_continue!(router().get_stream_id(ssrc, self.port, addr.ip()));
            //
            // if !self.rtp_status.contains_key(&(stream_id, ssrc)) {
            //     let router = router();
            //     let stream = or_continue!(router.get_stream(stream_id));
            //     let media_stream =
            //         or_continue!(stream.media_streams.iter().find(|item| item.ssrc == ssrc));
            //
            //     self.rtp_status.insert(
            //         (stream_id, ssrc),
            //         RTPStream::new(addr, media_stream.clone()),
            //     );
            // }

            // match muxed {
            //     MuxedRTP::RTCP(rtcp) => {
            //         self.handle_sender_report(stream_id, ssrc, rtcp, &mut buffer[..len], time)
            //     }
            //     MuxedRTP::RTP(rtp) => {
            //         self.handle_rtp_packet(stream_id, ssrc, rtp, &mut buffer[..len])
            //     }
            // }
        }
    }

    fn handle_sender_report(
        &mut self,
        stream_id: u32,
        ssrc: u32,
        header: RTCPHeader,
        payload: &mut [u8],
        time: Instant,
    ) {
        let rtp_stream = if let Some(rtp_stream) = self.rtp_status.get_mut(&(stream_id, ssrc)) {
            rtp_stream
        } else {
            return;
        };

        rtp_stream.last_sender_report = Some(time);

        let relay = callback().relay_rtcp;

        let mut rtcp_packet = Box::new(janus_plugin_rtcp {
            video: if rtp_stream.media_stream.content.is_video() {
                1
            } else {
                0
            },
            buffer: payload.as_mut_ptr().cast(),
            length: payload.len() as i16,
        });

        for sub in router().get_subscribers(stream_id) {
            relay(sub.handle, rtcp_packet.as_mut());
        }

        if header.payload_type == 200 {
            let mut header: u8 = 0;
            header |= 2 << 6;
            header |= 1;
            let epoch = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap();

            let mut packet = vec![header, 250, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
            packet[4..8].copy_from_slice(&(epoch.as_secs() as u32).to_be_bytes());
            packet[8..12].copy_from_slice(&(epoch.subsec_nanos() as u32).to_be_bytes());

            self.udp_socket.send_to(&packet, rtp_stream.addr).unwrap();
        }
    }

    fn send_nacks(&mut self, stream_id: u32, ssrc: u32, seq_nr: Wrapping<u16>) {
        let rtp_stream = if let Some(rtp_stream) = self.rtp_status.get_mut(&(stream_id, ssrc)) {
            rtp_stream
        } else {
            return;
        };

        let nacks = rtp_stream.push_sequence(seq_nr);
        if nacks.is_empty() {
            return;
        }

        let mut header: u8 = 0;
        header |= 2 << 6;
        header |= 1;
        let mut packet = vec![0u8; 12];
        packet[0] = header;
        packet[1] = 205;
        packet[2..4].copy_from_slice(&(2u16 + nacks.len() as u16).to_be_bytes());
        packet[8..12].copy_from_slice(&ssrc.to_be_bytes());

        packet.reserve(nacks.len() * 4);
        println!("Sending NACKS ({:?})", nacks.len());
        for nack in nacks {
            packet.extend(&nack.0.to_be_bytes());
            packet.extend(&nack.1.to_be_bytes());
        }

        self.udp_socket.send_to(&packet, rtp_stream.addr).unwrap();
    }

    fn handle_rtp_packet(
        &mut self,
        stream_id: u32,
        ssrc: u32,
        header: RTPHeader,
        payload: &mut [u8],
    ) {
        self.send_nacks(stream_id, ssrc, header.seq_nr);

        let rtp_stream = if let Some(rtp_stream) = self.rtp_status.get_mut(&(stream_id, ssrc)) {
            rtp_stream
        } else {
            return;
        };
        //
        // let relay = callback().relay_rtp;
        //
        // let mut rtp_packet = Box::new(janus_plugin_rtp {
        //     video: if rtp_stream.media_stream.content.is_video() {
        //         1
        //     } else {
        //         0
        //     },
        //     buffer: payload.as_mut_ptr().cast(),
        //     length: payload.len() as i16,
        //     extensions: janus_plugin_rtp_extensions {
        //         audio_level: 0,
        //         audio_level_vad: 0,
        //         video_rotation: 0,
        //         video_back_camera: 0,
        //         video_flipped: 0,
        //     },
        // });
        //
        // for sub in router().get_subscribers(stream_id) {
        //     relay(sub.handle, rtp_packet.as_mut());
        // }

        if let MediaStreamContent::Video(video) = &rtp_stream.media_stream.content {
            if video.codec == VideoCodec::H264 {
                let mut router = router_mut();
                let stream = or_return!(router.get_stream_mut(stream_id));
                let rtp_payload = or_return!(get_rtp_payload(payload));
                let buffer = stream.keyframe_buffer.entry(ssrc).or_default();
                if is_h264_keyframe(rtp_payload) {
                    println!("Got h264 keyframe!");
                    buffer.clear();
                }

                buffer.push(payload.to_vec());
            }
        }
    }
}

fn is_h264_keyframe<T: AsRef<[u8]>>(input: T) -> bool {
    let input = input.as_ref();
    if input.len() < 6 {
        return false;
    }

    let frag = input[0] & 0x1F;
    let nal = input[1] & 0x1F;

    if frag == 7 || ((frag == 28 || frag == 29) && nal == 7) {
        return true;
    }

    if frag == 24 {
        let mut offset = 1;
        while (offset + 2) < input.len() {
            let nal = input[offset + 2] & 0x1F;
            if nal == 7 {
                return true;
            }

            let packet_len = u16::from_be_bytes(input[offset..offset + 2].try_into().unwrap());
            offset += packet_len as usize;
        }
    }

    false
}
