use discortp::rtp::{RtpPacket, RtpType};

use crate::discord::crypto::{NONCE_SIZE, TAG_SIZE};

// Discord wants 48kHz
pub const SAMPLE_RATE: u32 = 48_000;

// 20ms of audio at 48kHz, 20 ms is apparently the most common frame size in Asterisk.
pub const NUM_SAMPLES: u32 = 960;

pub const RTP_VERSION: u8 = 2;
pub const RTP_PROFILE_TYPE: RtpType = RtpType::Dynamic(0x78);

pub const MAX_RTP_PACKET_SIZE: usize = 1450;
pub const MAX_OPUS_PAYLOAD_SIZE: usize =
    MAX_RTP_PACKET_SIZE - RtpPacket::minimum_packet_size() - TAG_SIZE - NONCE_SIZE;
