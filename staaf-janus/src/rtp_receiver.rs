use crate::{callback, or_continue, router};
use discortp::demux::Demuxed::Rtp;
use discortp::demux::{demux, Demuxed};
use discortp::rtcp::report::{
    MutableReceiverReportPacket, MutableReportBlockPacket, ReceiverReportPacket, ReportBlock,
    ReportBlockPacket, SenderInfo, SenderInfoIterable, SenderInfoPacket, SenderReport,
    SenderReportPacket,
};
use discortp::rtcp::RtcpType::ReceiverReport;
use discortp::rtcp::{RtcpPacket, RtcpType};
use discortp::rtp::RtpPacket;
use discortp::wrap::Wrap16;
use discortp::{FromPacket, Packet};
use janus_plugin::PluginRtpPacket;
use janus_plugin_sys::plugin::{janus_plugin_rtcp, janus_plugin_rtp, janus_plugin_rtp_extensions};
use std::collections::{HashMap, VecDeque};
use std::convert::TryInto;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Instant, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct RTPStream {
    sequence: Wrap16,
    addr: SocketAddr,
    stream_start: Instant,
    last_sender_report: Option<Instant>,
}

impl RTPStream {
    fn new(sequence: Wrap16, addr: SocketAddr) -> RTPStream {
        RTPStream {
            sequence,
            addr,
            stream_start: Instant::now(),
            last_sender_report: None,
        }
    }
}

pub struct RTPPacketQueue {
    packets: VecDeque<RtpPacket<'static>>,
    top: Wrap16,
    bottom: Wrap16,
    size: usize,
}

pub struct RTPReceiver {
    port: u16,
    udp_socket: UdpSocket,
    rtp_status: HashMap<(u32, u32), RTPStream>,
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
        while let Ok((len, addr)) = self.udp_socket.recv_from(&mut buffer) {
            // Get time as soon as possible before waiting on any locks
            let time = Instant::now();
            let demuxed = demux(&buffer[..len]);
            let ssrc = match &demuxed {
                Demuxed::Rtcp(RtcpPacket::SenderReport(sender_report)) => sender_report.get_ssrc(),
                Demuxed::Rtp(rtp) => rtp.get_ssrc(),
                _ => continue,
            };

            let stream_id = or_continue!(router().get_stream_id(ssrc, self.port, addr.ip()));

            if !self.rtp_status.contains_key(&(stream_id, ssrc)) {
                self.rtp_status
                    .insert((stream_id, ssrc), RTPStream::new(0.into(), addr));
            }

            match demuxed {
                Demuxed::Rtcp(RtcpPacket::SenderReport(sender_report)) => {
                    self.handle_sender_report(stream_id, ssrc, sender_report, time)
                }
                Demuxed::Rtp(rtp_packet) => self.handle_rtp_packet(stream_id, ssrc, rtp_packet),
                _ => {}
            }
        }
    }

    fn handle_sender_report(
        &mut self,
        stream_id: u32,
        ssrc: u32,
        report: SenderReportPacket,
        time: Instant,
    ) {
        let rtp_stream = if let Some(rtp_stream) = self.rtp_status.get_mut(&(stream_id, ssrc)) {
            rtp_stream
        } else {
            return;
        };

        rtp_stream.last_sender_report = Some(time);

        let sender_report = report.from_packet();
        let sender_info = SenderInfoPacket::new(report.packet()).unwrap();

        println!(
            "channel({}, ssrc={}) => {:?} | {:?}",
            stream_id, ssrc, sender_report, sender_info
        );

        let relay = callback().relay_rtcp;

        let mut buffer = Vec::from(report.packet());
        let mut rtcp_packet = Box::new(janus_plugin_rtcp {
            video: i8::from(ssrc != stream_id),
            buffer: buffer.as_mut_ptr().cast(),
            length: buffer.len() as i16,
        });

        for sub in router().get_subscribers(stream_id) {
            relay(sub.handle, rtcp_packet.as_mut());
        }

        let mut header: u8 = 0;
        header |= 2 << 6;
        header |= 1;
        let mut packet = vec![header, 250, 0, 0];
        packet.append(&mut sender_info.get_ntp_timestamp_second().to_be_bytes()[..].to_vec());
        packet.append(&mut sender_info.get_ntp_timestamp_fraction().to_be_bytes()[..].to_vec());

        self.udp_socket.send_to(&packet, rtp_stream.addr).unwrap();
    }

    fn handle_rtp_packet(&mut self, stream_id: u32, ssrc: u32, packet: RtpPacket) {
        let rtp_stream = if let Some(rtp_stream) = self.rtp_status.get_mut(&(stream_id, ssrc)) {
            rtp_stream
        } else {
            return;
        };

        if rtp_stream.sequence.0 >= packet.get_sequence().0
            || (rtp_stream.sequence + 100).0 >= (packet.get_sequence() + 100).0
        {
            return;
        }

        rtp_stream.sequence = packet.get_sequence();

        let relay = callback().relay_rtp;

        let mut buffer = Vec::from(packet.packet());
        let mut rtp_packet = Box::new(janus_plugin_rtp {
            video: i8::from(ssrc != stream_id),
            buffer: buffer.as_mut_ptr().cast(),
            length: buffer.len() as i16,
            extensions: janus_plugin_rtp_extensions {
                audio_level: 0,
                audio_level_vad: 0,
                video_rotation: 0,
                video_back_camera: 0,
                video_flipped: 0,
            },
        });

        for sub in router().get_subscribers(stream_id) {
            relay(sub.handle, rtp_packet.as_mut());
        }
    }
}
