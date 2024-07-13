use anyhow::{anyhow, bail};
use crypto_secretbox::{KeyInit, SecretBox};
use discortp::demux::{demux_mut, DemuxedMut};
use discortp::discord::{IpDiscoveryPacket, IpDiscoveryType, MutableIpDiscoveryPacket};
use discortp::rtp::MutableRtpPacket;
use discortp::{MutablePacket, Packet};
use log::debug;
use rand::{thread_rng, RngCore};
use std::net::IpAddr;
use std::ops::Range;
use std::str::FromStr;
use tokio::net::{ToSocketAddrs, UdpSocket};

use crate::constants::{RTP_PROFILE_TYPE, RTP_VERSION};

use super::crypto::{EncryptionMode, VoiceDecryption, VoiceEncryption};

pub struct VoiceDataChannel {
    pub public_addr: IpAddr,
    pub public_port: u16,
    pub ssrc: u32,
    sequence_no: u16,
    socket: UdpSocket,
    crypto: Option<(VoiceEncryption, VoiceDecryption)>,
    send_buf: Box<[u8; Self::VOICE_PACKET_MAX]>,
}

pub struct ReceivedRtpPacket {
    pub sequence_number: u16,
    pub timestamp: u32,
    pub ssrc: u32,
    pub buffer: Vec<u8>,
    pub data_range: Range<usize>,
}

pub struct ReceivedRtcpPacket {
    pub decrypted_buffer: Vec<u8>,
}

pub enum VoicePacket {
    Rtp(ReceivedRtpPacket),
    Rtcp(ReceivedRtcpPacket),
}

impl VoiceDataChannel {
    const VOICE_PACKET_MAX: usize = 1460;

    pub fn set_key(&mut self, mode: EncryptionMode, key: &[u8]) {
        let aead = SecretBox::new(key.into());

        self.crypto = Some((
            VoiceEncryption::new(mode, aead.clone()),
            VoiceDecryption::new(mode, aead),
        ));
    }

    pub async fn connect<A: ToSocketAddrs>(addr: A, ssrc: u32) -> anyhow::Result<Self> {
        // todo: ipv6?
        let udp = UdpSocket::bind("0.0.0.0:0").await?;
        udp.connect(addr).await?;

        let mut bytes = [0; IpDiscoveryPacket::const_packet_size()];
        {
            // Follow Discord's IP Discovery procedures, in case NAT tunnelling is needed.
            {
                let mut view = MutableIpDiscoveryPacket::new(&mut bytes[..]).expect(
                    "Too few bytes in 'bytes' for IPDiscovery packet.\
                    (Blame: IpDiscoveryPacket::const_packet_size()?)",
                );
                view.set_pkt_type(IpDiscoveryType::Request);
                view.set_length(70);
                view.set_ssrc(ssrc);
            }

            udp.send(&bytes).await?;
        }

        let (addr, port) = {
            let (len, _addr) = udp.recv_from(&mut bytes).await?;

            let view = IpDiscoveryPacket::new(&bytes[..len])
                .ok_or(anyhow!("illegal discovery response"))?;

            if view.get_pkt_type() != IpDiscoveryType::Response {
                return Err(anyhow!("Unexpected discovery response"));
            }

            // The public address is zero-terminated.
            let nul_byte_index = view
                .get_address_raw()
                .iter()
                .position(|&b| b == 0)
                .ok_or(anyhow!("Illegal public IP sent: Overflow"))?;

            let address_str = std::str::from_utf8(&view.get_address_raw()[..nul_byte_index])
                .map_err(|_| anyhow!("Illegal public IP sent: Not a string"))?;

            let address = IpAddr::from_str(address_str)
                .map_err(|e| anyhow!("Illegal public IP sent: {e:?}"))?;

            (address, view.get_port().into())
        };
        debug!("UDP voice channel to discord ready, resolved public addr is {addr}:{port}");

        Ok(Self {
            public_port: port,
            public_addr: addr,
            socket: udp,
            ssrc,
            sequence_no: thread_rng().next_u32() as u16,
            crypto: None,
            send_buf: Box::new([0; Self::VOICE_PACKET_MAX]),
        })
    }

    pub async fn send_voice(&mut self, timestamp: u32, voice: &[u8]) -> anyhow::Result<()> {
        let seq_no = self.sequence_no;
        self.sequence_no = seq_no.wrapping_add(1);

        let Some((encrypt, _)) = &mut self.crypto else {
            return Err(anyhow!("Crypto not set up"));
        };

        let payload_len = voice.len();

        let bytes = self.send_buf.as_mut_slice();
        {
            let mut packet = MutableRtpPacket::new(bytes).unwrap();
            packet.set_version(RTP_VERSION);
            packet.set_payload_type(RTP_PROFILE_TYPE);
            packet.set_sequence(seq_no.into());
            packet.set_timestamp(timestamp.into());
            packet.set_ssrc(self.ssrc.into());
            let payload = packet.payload_mut();
            payload[VoiceEncryption::TAG_LEN..(VoiceEncryption::TAG_LEN + payload_len)]
                .copy_from_slice(&voice);
        }

        let Ok(size) = encrypt.encrypt_packet(bytes, payload_len) else {
            return Err(anyhow!("Could not encrypt"));
        };
        self.socket.send(&bytes[..size]).await?;

        if self.sequence_no % 100 == 0 {
            println!("send_voice sent something")
        }

        Ok(())
    }

    pub async fn receive_packet(&mut self) -> anyhow::Result<VoicePacket> {
        let mut buffer = vec![0; Self::VOICE_PACKET_MAX];
        let len = self.socket.recv(&mut buffer).await?;
        buffer.truncate(len);

        let Some((_, ref decrypt)) = self.crypto else {
            bail!("Received packet, but crypto was not set up");
        };

        Ok(match demux_mut(&mut buffer) {
            DemuxedMut::Rtp(mut packet) => {
                let range = decrypt.decrypt_packet(&mut packet)?;

                let sequence = packet.get_sequence().into();
                let timestamp = packet.get_timestamp().into();
                let ssrc = packet.get_ssrc().into();
                VoicePacket::Rtp(ReceivedRtpPacket {
                    sequence_number: sequence,
                    timestamp,
                    ssrc,
                    buffer,
                    data_range: range,
                })
            }
            DemuxedMut::Rtcp(mut packet) => {
                let range = decrypt.decrypt_packet(&mut packet)?;
                let header_size = packet.packet().len() - packet.payload().len();

                buffer.drain(range.end..); // Remove suffix, if any
                buffer.drain(header_size..range.start); // Remove tag

                VoicePacket::Rtcp(ReceivedRtcpPacket {
                    decrypted_buffer: buffer,
                })
            }
            DemuxedMut::FailedParse(t) => {
                bail!("Failed decoding incoming packet at {t:?}");
            }
            DemuxedMut::TooSmall => {
                bail!("Illegal UDP packet from voice server.");
            }
        })
    }
}

impl std::fmt::Debug for VoicePacket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rtp(rtp) => f.debug_tuple("Rtp").field(&rtp.sequence_number).finish(),
            Self::Rtcp { .. } => f.debug_struct("Rtcp").finish(),
        }
    }
}
