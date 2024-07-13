use std::{
    collections::{hash_map::Entry, HashMap},
    ptr::null_mut,
    time::{Duration, SystemTime},
};

use chan_discord_common::{
    constants::SAMPLE_RATE,
    discord::rtp::VoicePacket,
    error::{ChanRes, DiscordError},
    utils::rtp::skip_over_extensions,
};
use log::{debug, warn};
use num_integer::Average;
use twilight_model::id::{marker::UserMarker, Id};

use asterisk::{astobj2::Ao2, formats::Format};
use asterisk_sys::bindings::{
    ast_frame, ast_frame__bindgen_ty_1, ast_frame__bindgen_ty_2, ast_frame_subclass,
    ast_frame_subclass__bindgen_ty_1, ast_frame_type_AST_FRAME_VOICE, timeval,
};

#[cfg(feature = "rtplog")]
use chan_discord_common::utils::rtp_log::RtpLog;

pub struct RtpReceiver {
    format: Ao2<Format>,
    user_id_to_ssrc: HashMap<Id<UserMarker>, u32>,
    ssrc_to_participant: HashMap<u32, OtherParticipant>,
    #[cfg(feature = "rtplog")]
    log: RtpLog,
}

struct OtherParticipant {
    decoder: opus::Decoder,
    time_synchronization: Option<(u32, SystemTime)>,
}

impl RtpReceiver {
    pub fn new() -> Self {
        Self {
            format: Format::slin48(),
            user_id_to_ssrc: HashMap::new(),
            ssrc_to_participant: HashMap::new(),
            #[cfg(feature = "rtplog")]
            log: RtpLog::new().unwrap(),
        }
    }

    pub fn map_user_id(&mut self, user: Id<UserMarker>, ssrc: u32) -> ChanRes<()> {
        match self.ssrc_to_participant.entry(ssrc) {
            Entry::Occupied(_) => {
                // ignore, nothing to do
            }
            Entry::Vacant(vacant) => {
                vacant.insert(OtherParticipant {
                    decoder: opus::Decoder::new(SAMPLE_RATE, opus::Channels::Stereo)
                        .map_err(|e| DiscordError::InternalError { source: e.into() })?,
                    time_synchronization: None,
                });

                // Since we have a user we better update the user id -> ssrc mapping as well
                self.user_id_to_ssrc.insert(user, ssrc);
            }
        };
        Ok(())
    }

    pub fn unmap_user_id(&mut self, user: Id<UserMarker>) {
        if let Some(ssrc) = self.user_id_to_ssrc.remove(&user) {
            self.ssrc_to_participant.remove(&ssrc);
        }
    }

    pub fn handle_packet(&mut self, packet: VoicePacket) -> Option<(Vec<i16>, ast_frame)> {
        match packet {
            VoicePacket::Rtp(packet) => {
                #[cfg(feature = "rtplog")]
                {
                    let data = &packet.buffer[packet.data_range.clone()];
                    self.log
                        .log_packet(packet.ssrc, packet.timestamp, packet.sequence_number, data)
                        .unwrap();
                }

                let Some(range) = skip_over_extensions(&packet.buffer, packet.data_range.clone())
                else {
                    debug!(
                        "Not enough of packet left after skipping over extensions, ssrc {}",
                        packet.ssrc
                    );
                    return None;
                };
                let data = &packet.buffer[range];

                let Some(participant) = self.ssrc_to_participant.get_mut(&packet.ssrc) else {
                    debug!(
                        "Received RTP packet from unknown sender, ssrc: {}",
                        packet.ssrc
                    );
                    return None;
                };

                // todo: We should definitely mix those packets, but it looks like there is no
                // synchronization opportunity?

                // We need to be sure to malloc this, as we indicate in the generate frame that
                // it should be freed by Asterisk.
                let mut voice = vec![0; 2 * 960];

                match participant.decoder.decode(data, &mut voice, false) {
                    Ok(actual_samples) => {
                        // Monoize the samples
                        for i in 0..actual_samples {
                            let left = voice[2 * i];
                            let right = voice[2 * i + 1];

                            voice[i] = left.average_ceil(&right);
                        }

                        voice.truncate(actual_samples);

                        let timestamp = match participant
                            .translate_packet_timestamp(packet.timestamp)
                            .duration_since(SystemTime::UNIX_EPOCH)
                        {
                            Ok(dur) => dur.as_millis().try_into().unwrap(),
                            Err(_) => 0,
                        };

                        let frame = ast_frame {
                            frametype: ast_frame_type_AST_FRAME_VOICE,
                            subclass: ast_frame_subclass {
                                __bindgen_anon_1: ast_frame_subclass__bindgen_ty_1 {
                                    format: self.format.as_ptr().cast(),
                                },
                                integer: 0,
                                frame_ending: 0,
                            },
                            datalen: (voice.len() * std::mem::size_of::<i16>()) as i32,
                            samples: actual_samples as i32,
                            mallocd: 0,
                            mallocd_hdr_len: 0,
                            offset: 0,
                            src: null_mut(),
                            data: ast_frame__bindgen_ty_1 {
                                ptr: voice.as_mut_ptr().cast(),
                            },
                            delivery: timeval {
                                tv_sec: timestamp / 1000,
                                tv_usec: (timestamp % 1000) * 1000,
                            },
                            frame_list: ast_frame__bindgen_ty_2 { next: null_mut() },
                            flags: 0,
                            ts: timestamp,
                            len: (1000 * actual_samples as i64) / (SAMPLE_RATE as i64),
                            seqno: packet.sequence_number as i32,
                            stream_num: packet.ssrc as i32,
                        };

                        return Some((voice, frame));
                    }
                    Err(e) => {
                        warn!("Could not decode voice data: {e}");
                    }
                }
            }
            VoicePacket::Rtcp(_packet) => {}
        };

        None
    }
}

impl OtherParticipant {
    fn translate_packet_timestamp(&mut self, timestamp: u32) -> SystemTime {
        match self.time_synchronization {
            Some((reference, time)) if reference < timestamp => {
                let delta = timestamp - reference;
                let delta_millis = 1000 * delta / SAMPLE_RATE;
                time + Duration::from_millis(delta_millis as u64)
            }
            _ => {
                let now = SystemTime::now();
                self.time_synchronization = Some((timestamp, now));
                now
            }
        }
    }
}
