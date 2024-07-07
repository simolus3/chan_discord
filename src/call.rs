use std::ffi::CStr;

use anyhow::anyhow;
use log::trace;
use tokio::sync::{mpsc, oneshot};
use twilight_gateway::{Event, MessageSender};
use twilight_model::id::{
    marker::{ChannelMarker, GuildMarker, UserMarker},
    Id,
};

use crate::{
    asterisk::{bindings::ast_control_frame_type_AST_CONTROL_ANSWER, channel::Channel},
    discord::voice_task::{VoiceEvent, VoiceTaskHandle},
    error::{ChanRes, DiscordError},
    utils::{request_channel, RequestReceiver, RequestSender},
};

pub struct CallHandle {
    requests: RequestSender<CallRequest, ChanRes<CallResponse>>,
}

pub enum CallRequest {
    JoinChannel,
    HangUp,
}

pub struct CallResponse {}

pub struct CallWorker {
    asterisk_channel: Channel,
    voice: VoiceTaskState,
    requests: RequestReceiver<CallRequest, ChanRes<CallResponse>>,
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
}

enum WorkerEvent {
    ClientRequest(Option<(CallRequest, oneshot::Sender<ChanRes<CallResponse>>)>),
    CallEvent(Option<VoiceEvent>),
}

impl CallWorker {
    pub fn new(
        asterisk_channel: Channel,
        server: Id<GuildMarker>,
        channel: Id<ChannelMarker>,
        user: Id<UserMarker>,
        sender: MessageSender,
        events: mpsc::Receiver<Event>,
    ) -> (Self, CallHandle) {
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
        };
        (worker, CallHandle { requests: send })
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
    ) {
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
            CallRequest::HangUp => {
                let voice = std::mem::replace(
                    &mut self.voice,
                    VoiceTaskState::ShuttingDown {
                        hung_up_locally: true,
                    },
                );
                if let VoiceTaskState::VoiceStarted { handle } = voice {
                    handle.leave_and_close().await;
                };
                let _ = response.send(Ok(CallResponse {}));
            }
        }
    }

    async fn handle_call_event(&mut self, event: VoiceEvent) {
        trace!("Handling call event: {event:?}");

        match event {
            VoiceEvent::Packet(_) => {}
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
    }

    pub async fn run(mut self) {
        let hung_up_locally = loop {
            if let VoiceTaskState::ShuttingDown { hung_up_locally } = &self.voice {
                break *hung_up_locally;
            }

            let event = Self::next_event(&mut self).await;
            match event {
                WorkerEvent::ClientRequest(req) => {
                    let Some((req, res)) = req else {
                        break true;
                    };
                    self.handle_request(req, res).await;
                }
                WorkerEvent::CallEvent(event) => {
                    let Some(event) = event else {
                        break false;
                    };
                    self.handle_call_event(event).await;
                }
            }
        };

        if !hung_up_locally {
            self.asterisk_channel.queue_hangup();
        }
    }
}
