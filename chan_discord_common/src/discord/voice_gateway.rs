use std::{net::IpAddr, ops::Add, time::Duration};

use anyhow::{anyhow, Context};
use futures_util::{SinkExt, StreamExt};
use log::{debug, trace, warn};
use rand::{thread_rng, RngCore};
use serenity_voice_model::{
    id::{GuildId, UserId},
    payload::{
        ClientConnect, ClientDisconnect, Heartbeat, Identify, Ready, SelectProtocol,
        SessionDescription, Speaking,
    },
    Event, ProtocolData,
};
use tokio::{
    net::TcpStream,
    sync::mpsc::{Receiver, Sender},
    task::JoinHandle,
    time::{sleep_until, Instant},
};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{http::Uri, stream::MaybeTlsStream, Message},
    WebSocketStream,
};

use super::crypto::EncryptionMode;

type WebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub struct GatewayConnection {
    socket_task: JoinHandle<()>,
    events: Receiver<ConnectionEvent>,
    commands: Sender<ConnectionCommand>,
}

#[derive(Debug)]
pub enum VoiceEvent {
    Ready(Ready),
    Speaking(Speaking),
    SessionDescription(SessionDescription),
    ClientConnect(ClientConnect),
    ClientDisconnect(ClientDisconnect),
    Closed,
}

enum ConnectionEvent {
    Opened,
    Received(Event),
    Closed,
}

enum ConnectionCommand {
    Send(Event),
    SetHeartbeatInterval(Duration),
    Close,
}

impl GatewayConnection {
    pub fn start(host: String) -> Self {
        let (events_tx, events_rx) = tokio::sync::mpsc::channel(8);
        let (command_tx, command_rx) = tokio::sync::mpsc::channel(8);

        let task = {
            tokio::spawn(async move {
                let res = Self::socket_task(host, command_rx, events_tx.clone()).await;
                if let Err(e) = res {
                    warn!("Discord voice gateway task failed: {e:#?}")
                }
                let _ = events_tx.send(ConnectionEvent::Closed);
            })
        };

        Self {
            socket_task: task,
            events: events_rx,
            commands: command_tx,
        }
    }

    pub async fn send_identify(
        &self,
        server_id: GuildId,
        user_id: UserId,
        session_id: String,
        token: String,
    ) -> anyhow::Result<()> {
        self.send(Event::Identify(Identify {
            server_id,
            session_id,
            token,
            user_id,
        }))
        .await
    }

    pub async fn send_select_protocol(
        &self,
        addr: IpAddr,
        port: u16,
        mode: EncryptionMode,
    ) -> anyhow::Result<()> {
        self.send(Event::SelectProtocol(SelectProtocol {
            data: ProtocolData {
                address: addr,
                mode: mode.name().to_string(),
                port,
            },
            protocol: "udp".to_string(),
        }))
        .await
    }

    pub async fn send(&self, event: Event) -> anyhow::Result<()> {
        self.commands.send(ConnectionCommand::Send(event)).await?;
        Ok(())
    }

    pub async fn close(&self) -> anyhow::Result<()> {
        self.commands.send(ConnectionCommand::Close).await?;
        Ok(())
    }

    pub async fn next_event(&mut self) -> anyhow::Result<VoiceEvent> {
        loop {
            return Ok(
                match self
                    .events
                    .recv()
                    .await
                    .ok_or(anyhow!("Event channel closed"))?
                {
                    ConnectionEvent::Opened => continue,
                    ConnectionEvent::Received(event) => match event {
                        Event::Ready(ready) => VoiceEvent::Ready(ready),
                        Event::SessionDescription(desc) => VoiceEvent::SessionDescription(desc),
                        Event::Speaking(speaking) => VoiceEvent::Speaking(speaking),
                        Event::Hello(hello) => {
                            self.commands
                                .send(ConnectionCommand::SetHeartbeatInterval(
                                    Duration::from_secs_f64(hello.heartbeat_interval / 1000.0),
                                ))
                                .await?;
                            continue;
                        }
                        Event::HeartbeatAck(_) => continue,
                        Event::ClientConnect(connect) => VoiceEvent::ClientConnect(connect),
                        Event::ClientDisconnect(disconnect) => {
                            VoiceEvent::ClientDisconnect(disconnect)
                        }
                        event => {
                            return Err(anyhow!("Unexpected event from server: {event:?}"));
                        }
                    },
                    ConnectionEvent::Closed => VoiceEvent::Closed,
                },
            );
        }
    }

    async fn socket_task(
        host: String,
        mut command_rx: Receiver<ConnectionCommand>,
        events_tx: Sender<ConnectionEvent>,
    ) -> anyhow::Result<()> {
        let uri = Uri::builder()
            .scheme("wss")
            .authority(host)
            .path_and_query("/?v=4")
            .build()
            .context("Could not build voice connection URL")?;
        trace!("Connecting to voice gateway at {uri}");
        let (mut conn, _) = connect_async(uri)
            .await
            .context("Could not connect to voice websocket gateway")?;

        let mut heartbeat_interval = Duration::from_secs(36000);
        let mut next_heartbeat = Instant::now().add(heartbeat_interval);

        loop {
            tokio::select! {
                command = command_rx.recv() => {
                    match command {
                        None => { return Ok(()) },
                        Some(command) => {
                            match command {
                                ConnectionCommand::Send(event) => {
                                    let str = serde_json::to_string(&event)?;
                                    trace!("Sending control message: {str}");
                                    if let Err(e) = conn.send(Message::Text(str)).await {
                                        return Err(e.into());
                                    }
                                },
                                ConnectionCommand::SetHeartbeatInterval(duration) => {
                                    heartbeat_interval = duration;
                                    next_heartbeat = Instant::now().add(heartbeat_interval);
                                },
                                ConnectionCommand::Close => {
                                    let _ = conn.close(None).await;
                                    return Ok(());
                                },
                            }
                        }
                    }
                },
                message = conn.next() => {
                    match message {
                        None => return Ok(()),
                        Some(msg) => {
                            let msg = msg?;
                            trace!("Voice control message: {msg:?}");
                            if matches!(&msg, Message::Close(_)) {
                                events_tx.send(ConnectionEvent::Closed).await?;
                                break Ok(());
                            }

                            let Ok(text) = msg.into_text() else {
                                continue;
                            };

                            let Ok(event) = serde_json::from_str(text.as_str()) else {
                                debug!("Unknown message on voice gateway");
                                continue;
                            };

                            events_tx.send(ConnectionEvent::Received(event)).await?
                        }
                    }
                },
                _ = sleep_until(next_heartbeat) => {
                    trace!("Sending heartbeat");
                    let nonce = thread_rng().next_u64();
                    let str = serde_json::to_string(&Event::Heartbeat(Heartbeat {nonce}))?;
                    if let Err(e) = conn.send(Message::Text(str)).await {
                        return Err(e.into());
                    }
                    next_heartbeat = Instant::now().add(heartbeat_interval);
                },
            }
        }
    }
}
