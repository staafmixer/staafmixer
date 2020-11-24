use std::convert::TryInto;

pub struct RtpHeader {
    pub payload_type: u8,
    pub seq_nr: u16,
    pub ssrc: u32,
    pub csrc_count: u8,
    pub extension: Option<RtpExtension>,
}

pub struct RtpExtension {
    pub id: u16,
    pub length: u16,
}

impl RtpHeader {
    pub fn parse(input: &[u8]) -> Option<RtpHeader> {
        if input.len() < 12 {
            return None;
        }

        let csrc_count = input[0] & 0b00001111;

        let extension = if (input[0] & 0b0001000) > 0 {
            let offset: usize = 12 + (csrc_count * 4) as usize;

            if (offset + 4) > input.len() {
                return None;
            }

            Some(RtpExtension {
                id: u16::from_be_bytes(input[offset..offset + 2].try_into().unwrap()),
                length: u16::from_be_bytes(input[offset + 2..offset + 4].try_into().unwrap()),
            })
        } else {
            None
        };

        Some(RtpHeader {
            payload_type: input[1],
            seq_nr: u16::from_be_bytes(input[2..4].try_into().unwrap()),
            ssrc: u32::from_be_bytes(input[8..12].try_into().unwrap()),
            csrc_count,
            extension,
        })
    }
}

pub struct RtcpHeader {
    pub payload_type: u8,
    pub ssrc: u32,
}

impl RtcpHeader {
    pub fn parse(input: &[u8]) -> Option<RtcpHeader> {
        if input.len() < 8 {
            return None;
        }

        Some(RtcpHeader {
            payload_type: input[1],
            ssrc: u32::from_be_bytes(input[4..8].try_into().unwrap()),
        })
    }
}

pub enum MuxedRtp {
    RTP(RtpHeader),
    RTCP(RtcpHeader),
}

impl MuxedRtp {
    pub fn ssrc(&self) -> u32 {
        match self {
            MuxedRtp::RTCP(rtcp) => rtcp.ssrc,
            MuxedRtp::RTP(rtp) => rtp.ssrc,
        }
    }

    pub fn seq_nr(&self) -> u16 {
        match self {
            MuxedRtp::RTCP(rtcp) => 0,
            MuxedRtp::RTP(rtp) => rtp.seq_nr,
        }
    }

    pub fn parse(input: &[u8]) -> Option<MuxedRtp> {
        if input.len() < 8 {
            return None;
        }

        match input[1] {
            194 | 195 | 200..=213 => RtcpHeader::parse(input).map(MuxedRtp::RTCP),
            _ => RtpHeader::parse(input).map(MuxedRtp::RTP),
        }
    }
}
