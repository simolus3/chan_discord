use std::fmt::Debug;
use std::str::FromStr;

use anyhow::{anyhow, bail};
use log::{info, trace, warn};
use serde::Serialize;
use serenity_voice_model::id::{GuildId, UserId};
use serenity_voice_model::payload::Speaking;
use serenity_voice_model::SpeakingState;
use tokio::sync::mpsc::{self, OwnedPermit, Receiver, Sender};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use twilight_gateway::{Event, MessageSender};
use twilight_model::id::marker::{ChannelMarker, GuildMarker, UserMarker};
use twilight_model::id::Id;

use crate::discord::crypto::EncryptionMode;
use crate::error::{ChanRes, DiscordError};
use crate::utils::{request_channel, RequestReceiver, RequestSender};

use super::rtp::{VoiceDataChannel, VoicePacket};
use super::voice_gateway;
use super::voice_gateway::GatewayConnection;

enum VoiceTaskRequest {
    Write(OutgoingVoicePacket),
    Close,
}

pub struct OutgoingVoicePacket {
    pub opus_payload: Vec<u8>,
    pub timestamp: u32,
}

type VoiceTaskResponse = ChanRes<()>;

#[derive(Debug)]
pub enum VoiceEvent {
    Packet(VoicePacket),
    UserJoined { ssrc: u32, user: Id<UserMarker> },
    Speaking { user: Id<UserMarker>, ssrc: u32 },
    UserLeft { user: Id<UserMarker> },
    FullyConnected,
    Closed,
}

pub struct VoiceTaskHandle {
    task: JoinHandle<()>,
    pub events: Receiver<VoiceEvent>,
    sender: RequestSender<VoiceTaskRequest, VoiceTaskResponse>,
}

#[derive(Default)]
struct WaitForConnectInfo {
    guild_id: Option<Id<GuildMarker>>,
    channel_id: Option<Id<ChannelMarker>>,
    session_id: Option<String>,
    token: Option<String>,
    endpoint: Option<String>,
}

enum VoiceTaskState {
    // The initial state - after announcing our intent of joining a voice channel on the global
    // gateway connection, we're waiting for Discord to tell us about the voice server to connect
    // to.
    WaitingForEvents(WaitForConnectInfo),
    WaitingForReady {
        gateway: Option<GatewayConnection>,
    },
    Connected {
        gateway: GatewayConnection,
        voice: VoiceDataChannel,
        has_session: bool,
    },
}

enum VoiceTaskEvent {
    IncomingRequest {
        request: VoiceTaskRequest,
        response: oneshot::Sender<VoiceTaskResponse>,
    },
    GlobalEvent {
        event: Event,
    },
    GatewayEvent {
        event: voice_gateway::VoiceEvent,
    },
    VoicePacket {
        packet: VoicePacket,
        permit: mpsc::OwnedPermit<VoiceEvent>,
    },
    NonFatalError {
        err: anyhow::Error,
    },
    Closed,
}

struct VoiceTaskRunner {
    user: Id<UserMarker>,
    guild: Id<GuildMarker>,
    channel: Id<ChannelMarker>,
    sender: MessageSender,
    state: VoiceTaskState,
    requests: RequestReceiver<VoiceTaskRequest, VoiceTaskResponse>,
    events: Sender<VoiceEvent>,
    gateway_events: mpsc::Receiver<Event>,
    close_requested: bool,
}

impl VoiceTaskHandle {
    pub async fn start_task(
        sender: MessageSender,
        gateway_events: mpsc::Receiver<Event>,
        user: Id<UserMarker>,
        guild: Id<GuildMarker>,
        channel: Id<ChannelMarker>,
    ) -> Self {
        let (event_sender, event_receiver) = mpsc::channel(32);
        let (send, receive) = request_channel();

        let runner = tokio::spawn(async move {
            let mut runner = VoiceTaskRunner {
                user,
                channel,
                guild,
                events: event_sender,
                sender,
                state: VoiceTaskState::default(),
                requests: receive,
                gateway_events,
                close_requested: false,
            };
            runner.run().await;
        });

        Self {
            task: runner,
            events: event_receiver,
            sender: send,
        }
    }

    pub async fn write(&self, packet: OutgoingVoicePacket) -> ChanRes<()> {
        self.sender
            .request(VoiceTaskRequest::Write(packet))
            .await
            .map_err(|e| DiscordError::InternalError { source: e.into() })?
    }

    pub async fn leave_and_close(self) {
        let _ = self.sender.request(VoiceTaskRequest::Close).await;
        let _ = self.task.await;
    }
}

impl VoiceTaskRunner {
    async fn run(&mut self) {
        if let Err(e) = self.register_join_intent() {
            warn!("Could not register intent to join voice channel: {e}");
            let _ = self.events.send(VoiceEvent::Closed).await;
            return;
        }

        while !self.close_requested {
            let event = self.wait_for_event().await;
            if let Err(e) = self.handle_event(event).await {
                warn!("Error in voice task runner: {e:#}");
                break;
            }
        }

        self.close().await;
    }

    async fn handle_event(&mut self, event: VoiceTaskEvent) -> anyhow::Result<()> {
        match event {
            VoiceTaskEvent::IncomingRequest { request, response } => match request {
                VoiceTaskRequest::Write(write) => {
                    let res = match &mut self.state {
                        VoiceTaskState::Connected {
                            voice,
                            has_session: true,
                            ..
                        } => voice
                            .send_voice(write.timestamp, &write.opus_payload)
                            .await
                            .map_err(|e| DiscordError::InternalError { source: e }),
                        _ => Err(DiscordError::InternalError {
                            source: anyhow!("Voice not set up yet."),
                        }),
                    };

                    let _ = response.send(res);
                }
                VoiceTaskRequest::Close => {
                    let _ = response.send(Ok(()));
                    self.close_requested = true;
                }
            },
            VoiceTaskEvent::GlobalEvent { event } => {
                if let VoiceTaskState::WaitingForEvents(waiting) = &mut self.state {
                    if waiting.apply(&event) {
                        let gateway = waiting.start_gateway(&self.user).await;
                        self.state = VoiceTaskState::WaitingForReady {
                            gateway: Some(gateway),
                        }
                    }
                }
            }
            VoiceTaskEvent::GatewayEvent { event } => {
                trace!("Handling voice gateway event: {event:?}");

                match event {
                    voice_gateway::VoiceEvent::Ready(event) => {
                        if let VoiceTaskState::WaitingForReady { gateway } = &mut self.state {
                            let encryption_mode = event
                                .modes
                                .iter()
                                .filter_map(|e| EncryptionMode::from_str(e).ok())
                                .max()
                                .ok_or(anyhow::anyhow!("Did not find an encryption mode"))?;

                            let Ok(voice) =
                                VoiceDataChannel::connect((event.ip, event.port), event.ssrc).await
                            else {
                                bail!("Could not connect to voice channel");
                            };

                            let gateway = gateway.take().unwrap();
                            let _ = gateway
                                .send_select_protocol(
                                    voice.public_addr,
                                    voice.public_port,
                                    encryption_mode,
                                )
                                .await;

                            self.state = VoiceTaskState::Connected {
                                gateway: gateway,
                                voice: voice,
                                has_session: false,
                            };
                        }
                    }
                    voice_gateway::VoiceEvent::Speaking(speaking) => {
                        if let Some(user) = speaking.user_id {
                            if speaking.delay.is_some_and(|x| x != 0) {
                                info!("Received interesting speaking event, delay not zero: {speaking:?}");
                            }

                            let _ = self
                                .events
                                .send(VoiceEvent::Speaking {
                                    user: Id::new(user.0),
                                    ssrc: speaking.ssrc,
                                })
                                .await;
                        }
                    }
                    voice_gateway::VoiceEvent::SessionDescription(desc) => {
                        if let VoiceTaskState::Connected {
                            gateway,
                            voice,
                            has_session,
                        } = &mut self.state
                        {
                            let Ok(mode) = EncryptionMode::from_str(&desc.mode) else {
                                bail!("Unknown encryption mode: {}", desc.mode);
                            };
                            voice.set_key(mode, desc.secret_key.as_slice());

                            if !*has_session {
                                // We need to send an empty listen packet to receive audio, see
                                // https://github.com/discord/discord-api-docs/issues/808
                                gateway
                                    .send(serenity_voice_model::Event::Speaking(Speaking {
                                        delay: Some(0),
                                        speaking: SpeakingState::MICROPHONE,
                                        ssrc: voice.ssrc,
                                        user_id: None,
                                    }))
                                    .await?;
                            }

                            *has_session = true;
                            let _ = self.events.send(VoiceEvent::FullyConnected).await;
                        }
                    }
                    voice_gateway::VoiceEvent::ClientConnect(connect) => {
                        let _ = self
                            .events
                            .send(VoiceEvent::UserJoined {
                                ssrc: connect.audio_ssrc,
                                user: Id::new(connect.user_id.0),
                            })
                            .await;
                    }
                    voice_gateway::VoiceEvent::ClientDisconnect(disconnect) => {
                        if disconnect.user_id.0 == self.user.get() {
                            self.close_requested = true;
                        }

                        let _ = self
                            .events
                            .send(VoiceEvent::UserLeft {
                                user: Id::new(disconnect.user_id.0),
                            })
                            .await;
                    }
                    voice_gateway::VoiceEvent::Closed => {
                        self.close_requested = true;
                    }
                }
            }
            VoiceTaskEvent::VoicePacket { packet, permit } => {
                permit.send(VoiceEvent::Packet(packet));
            }
            VoiceTaskEvent::Closed => {
                self.close_requested = true;
            }
            VoiceTaskEvent::NonFatalError { err } => {
                warn!("Error on data channel: {err:?}");
            }
        }

        Ok(())
    }

    async fn wait_for_event(&mut self) -> VoiceTaskEvent {
        let (gateway, rtp) = self.state.sockets_mut();
        let events = &mut self.events;

        tokio::select! {
            request = self.requests.request() => {
                let Some((req, chan)) = request else {
                    return VoiceTaskEvent::Closed;
                };

                VoiceTaskEvent::IncomingRequest { request: req, response: chan }
            },
            gateway_event = self.gateway_events.recv() => {
                let Some(event) = gateway_event else {
                    return VoiceTaskEvent::Closed;
                };

                VoiceTaskEvent::GlobalEvent { event: event }
            },
            voice_event = Self::next_gateway_event(gateway) => {
                match voice_event {
                    Ok(event) => VoiceTaskEvent::GatewayEvent { event: event },
                    Err(e) => {
                        warn!("Error from voice gateway: {e}");
                        VoiceTaskEvent::Closed
                    },
                }
            },
            packet = Self::next_data_event(rtp, events) => {
                match packet {
                    Ok((packet, permit)) => VoiceTaskEvent::VoicePacket{
                        packet,
                        permit,
                    },
                    Err(e) => VoiceTaskEvent::NonFatalError {err:e},
                }
            },
        }
    }

    async fn close(&mut self) {
        trace!("Closing voice task runner");
        let (gateway, _) = self.state.sockets_mut();
        if let Some(gateway) = gateway {
            let _ = gateway.close().await;
        }

        let _ = self.register_leave_intent();
    }

    async fn next_gateway_event(
        gateway: Option<&mut GatewayConnection>,
    ) -> anyhow::Result<voice_gateway::VoiceEvent> {
        match gateway {
            Some(gateway) => gateway.next_event().await,
            None => futures_util::future::pending().await,
        }
    }

    async fn next_data_event(
        data: Option<&mut VoiceDataChannel>,
        events: &mpsc::Sender<VoiceEvent>,
    ) -> anyhow::Result<(VoicePacket, OwnedPermit<VoiceEvent>)> {
        let Ok(permit) = events.clone().reserve_owned().await else {
            futures_util::future::pending::<()>().await;
            unreachable!()
        };

        Ok(match data {
            Some(channel) => (channel.receive_packet().await?, permit),
            None => futures_util::future::pending().await,
        })
    }

    fn register_join_intent(&self) -> anyhow::Result<()> {
        self.sender.send(serde_json::to_string(&VoiceStateUpdate {
            op: 4,
            d: UpdateRequest {
                guild_id: self.guild,
                channel_id: Some(self.channel),
                self_deaf: false,
                self_mute: false,
            },
        })?)?;

        Ok(())
    }

    fn register_leave_intent(&self) -> anyhow::Result<()> {
        self.sender.send(serde_json::to_string(&VoiceStateUpdate {
            op: 4,
            d: UpdateRequest {
                guild_id: self.guild,
                channel_id: None,
                self_deaf: false,
                self_mute: false,
            },
        })?)?;

        Ok(())
    }
}

impl VoiceTaskState {
    pub fn sockets_mut(
        &mut self,
    ) -> (
        Option<&mut GatewayConnection>,
        Option<&mut VoiceDataChannel>,
    ) {
        match self {
            VoiceTaskState::WaitingForReady { gateway, .. } => (gateway.as_mut(), None),
            VoiceTaskState::Connected { gateway, voice, .. } => (Some(gateway), Some(voice)),
            _ => (None, None),
        }
    }
}

impl Default for VoiceTaskState {
    fn default() -> Self {
        Self::WaitingForEvents(Default::default())
    }
}

impl WaitForConnectInfo {
    fn apply(&mut self, event: &Event) -> bool {
        match event {
            Event::VoiceStateUpdate(update) => {
                self.guild_id = update.guild_id;
                self.channel_id = update.channel_id;
                self.session_id = Some(update.session_id.clone());
            }
            Event::VoiceServerUpdate(update) => {
                self.endpoint = update.endpoint.clone();
                self.token = Some(update.token.clone());
            }
            _ => {}
        };

        self.is_complete()
    }

    fn is_complete(&self) -> bool {
        self.token.is_some()
            && self.endpoint.is_some()
            && self.guild_id.is_some()
            && self.channel_id.is_some()
            && self.session_id.is_some()
    }

    async fn start_gateway(&mut self, user: &Id<UserMarker>) -> GatewayConnection {
        let gateway = GatewayConnection::start(self.endpoint.take().unwrap());
        let _ = gateway
            .send_identify(
                GuildId(self.guild_id.unwrap().get()),
                UserId(user.get()),
                self.session_id.as_ref().unwrap().clone(),
                self.token.as_ref().unwrap().clone(),
            )
            .await;

        gateway
    }
}

#[derive(Serialize)]
struct UpdateRequest {
    guild_id: Id<GuildMarker>,
    channel_id: Option<Id<ChannelMarker>>,
    self_mute: bool,
    self_deaf: bool,
}

#[derive(Serialize)]
struct VoiceStateUpdate {
    op: usize,
    d: UpdateRequest,
}

impl Debug for OutgoingVoicePacket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutgoingVoicePacket")
            .field("opus_payload (len)", &self.opus_payload.len())
            .field("timestamp", &self.timestamp)
            .finish_non_exhaustive()
    }
}
