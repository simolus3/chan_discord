use std::ffi::CStr;

use anyhow::anyhow;
use chan_discord_common::{
    constants::{MAX_OPUS_PAYLOAD_SIZE, NUM_SAMPLES, SAMPLE_RATE},
    discord::voice_task::{OutgoingVoicePacket, VoiceEvent, VoiceTaskHandle},
    error::{ChanRes, DiscordError},
    utils::{request_channel, RequestReceiver, RequestSender},
};
use discortp::wrap::Wrap32;
use log::{trace, warn};
use rand::{thread_rng, Rng};
use tokio::sync::{mpsc, oneshot};
use twilight_gateway::{Event, MessageSender};
use twilight_model::id::{
    marker::{ChannelMarker, GuildMarker, UserMarker},
    Id,
};

use asterisk::{astobj2::Ao2, channel::Channel};
use asterisk_sys::bindings::{ast_control_frame_type_AST_CONTROL_ANSWER, ast_frame};

use crate::{
    queue_thread::{ChannelWriteKind, QueueThread},
    rtp_receiver::RtpReceiver,
};

pub struct CallHandle {
    requests: RequestSender<CallRequest, ChanRes<CallResponse>>,
    encoder: opus::Encoder,
    timestamp: Wrap32,
}

#[derive(Debug)]
pub enum CallRequest {
    JoinChannel,
    HangUp,
    WriteFrame(OutgoingVoicePacket),
    FixUp { new_channel: Ao2<Channel> },
}

#[derive(Debug)]
pub struct CallResponse {}

pub struct CallWorker {
    asterisk_channel: Ao2<Channel>,
    voice: VoiceTaskState,
    requests: RequestReceiver<CallRequest, ChanRes<CallResponse>>,
    rtp: RtpReceiver,
    queue_thread: QueueThread,
}

enum VoiceTaskState {
    Prepare {
        server: Id<GuildMarker>,
        channel: Id<ChannelMarker>,
        user: Id<UserMarker>,
        events: mpsc::Receiver<Event>,
        sender: MessageSender,
    },
    VoiceStarted {
        handle: VoiceTaskHandle,
    },
    ShuttingDown {
        hung_up_locally: bool,
    },
}

impl CallHandle {
    pub fn parse_destination_addr(str: &CStr) -> Option<(Id<GuildMarker>, Id<ChannelMarker>)> {
        let str = str.to_str().ok()?;

        let mut split = str.split('/');
        let guild = Id::new(split.next()?.parse::<u64>().ok()?);
        let channel = Id::new(split.next()?.parse::<u64>().ok()?);

        if split.next().is_some() {
            // We only want two elements
            return None;
        }

        Some((guild, channel))
    }

    fn request(&self, request: CallRequest) -> ChanRes<CallResponse> {
        let res = self
            .requests
            .request_blocking(request)
            .map_err(|e| DiscordError::InternalError { source: e.into() })??;
        Ok(res)
    }

    pub fn hangup(&self) -> ChanRes<()> {
        self.request(CallRequest::HangUp)?;
        Ok(())
    }

    pub fn start_joining(&self) -> ChanRes<()> {
        self.request(CallRequest::JoinChannel)?;
        Ok(())
    }

    pub fn fixup(&self, new_channel: Ao2<Channel>) -> ChanRes<()> {
        self.request(CallRequest::FixUp { new_channel })?;
        Ok(())
    }

    pub fn write_frame(&mut self, frame: &ast_frame) -> ChanRes<()> {
        let timestamp = self.timestamp;
        self.timestamp += NUM_SAMPLES;

        let raw_data = unsafe {
            std::slice::from_raw_parts(frame.data.ptr.cast::<i16>(), (frame.datalen / 2) as usize)
        };

        let res = self
            .encoder
            .encode_vec(raw_data, MAX_OPUS_PAYLOAD_SIZE)
            .map_err(|_| DiscordError::EncodeError)?;
        let res = self.request(CallRequest::WriteFrame(OutgoingVoicePacket {
            opus_payload: res,
            timestamp: timestamp.into(),
        }));
        res?;
        Ok(())
    }
}

#[derive(Debug)]
enum WorkerEvent {
    ClientRequest(Option<(CallRequest, oneshot::Sender<ChanRes<CallResponse>>)>),
    CallEvent(Option<VoiceEvent>),
}

impl CallWorker {
    pub fn new(
        asterisk_channel: Ao2<Channel>,
        server: Id<GuildMarker>,
        channel: Id<ChannelMarker>,
        user: Id<UserMarker>,
        sender: MessageSender,
        events: mpsc::Receiver<Event>,
    ) -> ChanRes<(Self, CallHandle)> {
        let rng = &mut thread_rng();
        let initial_timestamp = rng.gen::<u32>();

        let encoder =
            opus::Encoder::new(SAMPLE_RATE, opus::Channels::Mono, opus::Application::Voip)
                .map_err(|e| DiscordError::InternalError {
                    source: anyhow!("Could not create opus decoder: {e:?}"),
                })?;

        let (send, recv) = request_channel();

        let worker = Self {
            asterisk_channel,
            voice: VoiceTaskState::Prepare {
                server,
                channel,
                user,
                sender,
                events,
            },
            requests: recv,
            rtp: RtpReceiver::new(),
            queue_thread: super::queue_thread(),
        };

        Ok((
            worker,
            CallHandle {
                requests: send,
                encoder,

                timestamp: initial_timestamp.into(),
            },
        ))
    }

    async fn call_event(state: &mut VoiceTaskState) -> Option<VoiceEvent> {
        match state {
            VoiceTaskState::VoiceStarted { handle } => handle.events.recv().await,
            _ => std::future::pending().await,
        }
    }

    async fn next_event(&mut self) -> WorkerEvent {
        tokio::select! {
            request = self.requests.request() => {
                WorkerEvent::ClientRequest(request)
            },
            event = Self::call_event(&mut self.voice) => {
                WorkerEvent::CallEvent(event)
            }
        }
    }

    async fn handle_request(
        &mut self,
        request: CallRequest,
        response: oneshot::Sender<ChanRes<CallResponse>>,
    ) -> ChanRes<()> {
        match request {
            CallRequest::JoinChannel => {
                let voice = std::mem::replace(
                    &mut self.voice,
                    VoiceTaskState::ShuttingDown {
                        hung_up_locally: false,
                    },
                );

                let res = match voice {
                    VoiceTaskState::Prepare {
                        server,
                        channel,
                        user,
                        events,
                        sender,
                    } => {
                        let handle = VoiceTaskHandle::start_task(
                            sender.clone(),
                            events,
                            user,
                            server,
                            channel,
                        )
                        .await;
                        self.voice = VoiceTaskState::VoiceStarted { handle: handle };
                        Ok(CallResponse {})
                    }
                    _ => {
                        self.voice = voice;
                        Err(DiscordError::InternalError {
                            source: anyhow!("Tried to call same channel twice?!"),
                        })
                    }
                };

                let _ = response.send(res);
            }
            CallRequest::WriteFrame(packet) => {
                let res = match &self.voice {
                    VoiceTaskState::VoiceStarted { handle } => handle.write(packet).await,
                    _ => Err(DiscordError::InternalError {
                        source: anyhow!("Call not connected yet"),
                    }),
                }
                .map(|_| CallResponse {});
                let _ = response.send(res);
            }
            CallRequest::HangUp => {
                let voice = std::mem::replace(
                    &mut self.voice,
                    VoiceTaskState::ShuttingDown {
                        hung_up_locally: true,
                    },
                );
                if let VoiceTaskState::VoiceStarted { handle } = voice {
                    trace!("Stopping discord voice task");
                    handle.leave_and_close().await;
                };
                let _ = response.send(Ok(CallResponse {}));
            }
            CallRequest::FixUp { new_channel } => {
                self.asterisk_channel = new_channel;
            }
        }

        Ok(())
    }

    async fn handle_call_event(&mut self, event: VoiceEvent) -> ChanRes<()> {
        match event {
            VoiceEvent::Packet(packet) => {
                if let Some((backing, new_frame)) = self.rtp.handle_packet(packet) {
                    self.queue_thread.request(
                        self.asterisk_channel.clone(),
                        ChannelWriteKind::Frame {
                            backing_memory: backing,
                            frame: new_frame,
                        },
                    )?;
                }
            }
            VoiceEvent::UserJoined { ssrc, user } => {
                trace!("User {user} joined with {ssrc}");
                if let Err(e) = self.rtp.map_user_id(user, ssrc) {
                    warn!("Could not add discord user to mixer: {e}");
                }
            }
            VoiceEvent::UserLeft { user } => {
                trace!("User left: {user}");

                self.rtp.unmap_user_id(user);
            }
            VoiceEvent::Speaking { user, ssrc } => {
                trace!("User speaking: {user}, ssrc: {ssrc}");
                if let Err(e) = self.rtp.map_user_id(user, ssrc) {
                    warn!("Could not add discord user to mixer: {e}");
                }
            }
            VoiceEvent::FullyConnected => {
                self.asterisk_channel
                    .queue_control(ast_control_frame_type_AST_CONTROL_ANSWER);
            }
            VoiceEvent::Closed => {
                self.voice = VoiceTaskState::ShuttingDown {
                    hung_up_locally: false,
                };
            }
        }

        Ok(())
    }

    pub async fn run(mut self) {
        let hung_up_locally = loop {
            if let VoiceTaskState::ShuttingDown { hung_up_locally } = &self.voice {
                break *hung_up_locally;
            }

            let event = Self::next_event(&mut self).await;
            let res = match event {
                WorkerEvent::ClientRequest(req) => {
                    let Some((req, res)) = req else {
                        break true;
                    };
                    self.handle_request(req, res).await
                }
                WorkerEvent::CallEvent(event) => {
                    let Some(event) = event else {
                        break false;
                    };
                    self.handle_call_event(event).await
                }
            };

            if let Err(e) = res {
                warn!("Call stopping due to fatal error! {e:?}");
                break false;
            }
        };

        trace!("Ending call. Hung up locally: {hung_up_locally}");
        if !hung_up_locally {
            self.asterisk_channel.queue_hangup();
        }
    }
}
