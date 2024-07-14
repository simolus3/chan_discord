use std::{
    collections::{hash_map::Entry, HashMap},
    ptr::null_mut,
    time::{Duration, Instant},
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

use asterisk::{
    astobj2::Ao2,
    formats::Format,
    jitterbuffer::{JitterBuffer, JitterBufferErr},
};
use asterisk_sys::bindings::{
    ast_frame, ast_frame__bindgen_ty_1, ast_frame__bindgen_ty_2, ast_frame_subclass,
    ast_frame_subclass__bindgen_ty_1, ast_frame_type_AST_FRAME_VOICE, jb_conf, timeval,
};

#[cfg(feature = "rtplog")]
use chan_discord_common::utils::rtp_log::RtpLog;

pub struct RtpReceiver {
    format: Ao2<Format>,
    user_id_to_ssrc: HashMap<Id<UserMarker>, u32>,
    ssrc_to_participant: HashMap<u32, OtherParticipant>,
    known_next: Option<KnownNextFrameTime>,
    jb_conf: jb_conf,
    #[cfg(feature = "rtplog")]
    log: RtpLog,
}

pub enum FetchPacketResult {
    PacketAvailable {
        underlying_data: Vec<i16>,
        frame: ast_frame,
    },
    CheckBackLater {
        time: Instant,
    },
    NoneQueued,
}

unsafe impl Send for FetchPacketResult {}

struct OtherParticipant {
    decoder: opus::Decoder,
    initial_timestamp: Option<u32>,
    jitterbuf: Option<JitterBuffer<Vec<i16>>>,
    last_voice_length: Duration,
}

#[derive(Clone, Copy)]
struct KnownNextFrameTime {
    due: Instant,
    ssrc: u32,
}

impl RtpReceiver {
    const ASSUMED_VOICE_LENGTH: Duration = Duration::from_millis(20);

    pub fn new() -> Self {
        Self {
            format: Format::slin48(),
            user_id_to_ssrc: HashMap::new(),
            ssrc_to_participant: HashMap::new(),
            known_next: None,
            jb_conf: jb_conf {
                max_jitterbuf: 100,
                resync_threshold: 1000,
                max_contig_interp: 0,
                target_extra: 40,
            },
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
                    jitterbuf: None,
                    initial_timestamp: None,
                    last_voice_length: Self::ASSUMED_VOICE_LENGTH,
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

            if let Some(known) = &mut self.known_next {
                if known.ssrc == ssrc {
                    self.known_next = None;
                }
            }
        }
    }

    fn next_frame_time(&mut self) -> Option<KnownNextFrameTime> {
        match self.known_next {
            Some(known) => Some(known),
            None => {
                let map = &self.ssrc_to_participant;
                let (ssrc, time) = map
                    .into_iter()
                    .filter_map(|(ssrc, entry)| {
                        Some((ssrc, entry.jitterbuf.as_ref()?.next_frame()?))
                    })
                    .min_by_key(|(_, time)| *time)?;

                let time = KnownNextFrameTime {
                    due: time,
                    ssrc: *ssrc,
                };
                self.known_next = Some(time);
                Some(time)
            }
        }
    }

    pub fn fetch_packet(&mut self) -> FetchPacketResult {
        let Some(time) = self.next_frame_time() else {
            return FetchPacketResult::NoneQueued;
        };

        if time.due > Instant::now() {
            return FetchPacketResult::CheckBackLater { time: time.due };
        }

        let mut frames = vec![];
        for entry in self.ssrc_to_participant.values_mut() {
            let Some(jitterbuf) = &mut entry.jitterbuf else {
                continue;
            };

            let frame = loop {
                break match jitterbuf.get(entry.last_voice_length) {
                    Ok(frame) => Some(frame),
                    Err(e) => {
                        use asterisk::jitterbuffer::JitterBufferErr;

                        match e {
                            JitterBufferErr::Empty
                            | JitterBufferErr::Scheduled
                            | JitterBufferErr::NoFrame
                            | JitterBufferErr::Interpolate => None,
                            JitterBufferErr::Drop { frame } => {
                                drop(frame);
                                continue;
                            }
                        }
                    }
                };
            };

            if let Some(frame) = frame {
                frames.push(frame);
            }
        }

        if frames.is_empty() {
            return FetchPacketResult::NoneQueued;
        }

        let len = (&frames).into_iter().map(|f| f.data.len()).min().unwrap();
        let mut mixed = vec![0i16; len];
        for frame in frames {
            for (i, sample) in frame.data.into_iter().enumerate() {
                mixed[i] = mixed[i].saturating_add(sample);
            }
        }

        FetchPacketResult::PacketAvailable {
            frame: ast_frame {
                frametype: ast_frame_type_AST_FRAME_VOICE,
                subclass: ast_frame_subclass {
                    __bindgen_anon_1: ast_frame_subclass__bindgen_ty_1 {
                        format: self.format.as_ptr().cast(),
                    },
                    integer: 0,
                    frame_ending: 0,
                },
                datalen: (mixed.len() * std::mem::size_of::<i16>()) as i32,
                samples: mixed.len() as i32,
                mallocd: 0,
                mallocd_hdr_len: 0,
                offset: 0,
                src: null_mut(),
                data: ast_frame__bindgen_ty_1 {
                    ptr: mixed.as_mut_ptr().cast(),
                },
                delivery: timeval {
                    tv_sec: 0,
                    tv_usec: 0,
                },
                frame_list: ast_frame__bindgen_ty_2 { next: null_mut() },
                flags: 0,
                ts: 0,
                len: (1000 * len as i64) / (SAMPLE_RATE as i64),
                seqno: 0,
                stream_num: 0,
            },
            underlying_data: mixed,
        }
    }

    pub fn handle_packet(&mut self, packet: VoicePacket) {
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
                    return;
                };
                let data = &packet.buffer[range];

                let Some(participant) = self.ssrc_to_participant.get_mut(&packet.ssrc) else {
                    debug!(
                        "Received RTP packet from unknown sender, ssrc: {}",
                        packet.ssrc
                    );
                    return;
                };

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

                        let duration = Duration::from_millis(
                            (1000 * actual_samples as u64) / (SAMPLE_RATE as u64),
                        );
                        participant.last_voice_length = duration;
                        let jitterbuf = participant
                            .jitterbuf
                            .get_or_insert_with(|| JitterBuffer::new(&mut self.jb_conf));
                        let base_timestamp = *participant
                            .initial_timestamp
                            .get_or_insert(packet.timestamp);

                        let res = jitterbuf.put(
                            Box::new(voice),
                            asterisk::jitterbuffer::JitterFrameType::Voice,
                            duration,
                            // In RTP, the timestamp is measured in samples, but we want to measure
                            // it in milliseconds.
                            (1000 * (packet.timestamp - base_timestamp) as i64)
                                / (SAMPLE_RATE as i64),
                        );

                        if matches!(res, Err(JitterBufferErr::Scheduled)) {
                            // The expected time for the next frame has changed.
                            let Some(time) = jitterbuf.next_frame() else {
                                return;
                            };

                            if let Some(known) = &mut self.known_next {
                                if known.ssrc == packet.ssrc {
                                    known.due = time;
                                } else if time < known.due {
                                    known.due = time;
                                    known.ssrc = packet.ssrc;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Could not decode voice data: {e}");
                    }
                }
            }
            VoicePacket::Rtcp(_packet) => {}
        };
    }
}
