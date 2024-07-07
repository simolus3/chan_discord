use std::{collections::HashMap, thread::JoinHandle};

use serenity_voice_model::id::GuildId;
use tokio::{runtime, sync::mpsc};
use twilight_gateway::Event;
use twilight_model::id::{
    marker::{ChannelMarker, GuildMarker},
    Id,
};

use crate::{
    asterisk::channel::Channel,
    call::{CallHandle, CallWorker},
    discord::Discord,
    error::{ChanRes, DiscordError},
    utils::{request_channel, RequestReceiver, RequestSender},
};

/// Thread using an asynchronous Tokio runtime to manage Discord gateway web sockets as well as the
/// RTP sockets.
///
/// We generally prefer to keep everything async, but some Asterisk APIs (e.g. writing to channels)
/// require synchronous calls - in these cases, we can use channels to block the calling thread.
pub struct DiscordThread {
    handle: Option<JoinHandle<()>>,
    send: RequestSender<ThreadRequest, ChanRes<ThreadResponse>>,
}

enum ThreadRequest {
    Setup {
        token: String,
    },
    PrepareCall {
        asterisk_channel: Channel,
        server: Id<GuildMarker>,
        channel: Id<ChannelMarker>,
    },
    Stop,
}

enum ThreadResponse {
    Empty,
    CallPrepared { call: CallHandle },
}

impl DiscordThread {
    pub fn start(token: String) -> ChanRes<Self> {
        let (send, mut recv) = request_channel::<ThreadRequest, ChanRes<ThreadResponse>>();

        let handle = std::thread::Builder::new()
            .name("chan_discord_worker".to_string())
            .spawn(move || {
                let runtime = runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();

                runtime.block_on(async move {
                    let (request, response) = recv.request().await.unwrap();
                    let ThreadRequest::Setup { token } = request else {
                        return;
                    };

                    let mut worker = match DiscordThreadWorker::setup(token, recv).await {
                        Ok(worker) => worker,
                        Err(e) => {
                            let _ = response.send(Err(e));
                            return;
                        }
                    };
                    let _ = response.send(Ok(ThreadResponse::Empty));
                    worker.run().await;
                });
            })
            .map_err(|e| DiscordError::InternalError { source: e.into() })?;

        let thread = Self {
            handle: Some(handle),
            send,
        };
        thread.request(ThreadRequest::Setup { token })?;
        Ok(thread)
    }

    pub fn prepare_call(
        &self,
        asterisk: Channel,
        server: Id<GuildMarker>,
        channel: Id<ChannelMarker>,
    ) -> ChanRes<CallHandle> {
        let response = self.request(ThreadRequest::PrepareCall {
            asterisk_channel: asterisk,
            server,
            channel,
        })?;

        match response {
            ThreadResponse::CallPrepared { call } => Ok(call),
            _ => panic!("Expected call response"),
        }
    }

    fn request(&self, request: ThreadRequest) -> ChanRes<ThreadResponse> {
        self.send
            .request_blocking(request)
            .map_err(|e| DiscordError::InternalError { source: e.into() })?
    }
}

impl Drop for DiscordThread {
    fn drop(&mut self) {
        let _ = self.request(ThreadRequest::Stop);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

struct DiscordThreadWorker {
    recv: RequestReceiver<ThreadRequest, ChanRes<ThreadResponse>>,
    discord: Discord,
}

impl DiscordThreadWorker {
    async fn setup(
        token: String,
        recv: RequestReceiver<ThreadRequest, ChanRes<ThreadResponse>>,
    ) -> ChanRes<Self> {
        let discord = Discord::start(token).await?;
        Ok(Self { discord, recv })
    }

    async fn run(&mut self) {
        loop {
            let Some((request, response)) = self.recv.request().await else {
                break;
            };

            match request {
                ThreadRequest::Setup { .. } => {
                    panic!("Should have been handled in setup");
                }
                ThreadRequest::Stop => {
                    let _ = response.send(Ok(ThreadResponse::Empty));
                    break;
                }
                ThreadRequest::PrepareCall {
                    asterisk_channel,
                    server,
                    channel,
                } => {
                    let Some(events) = self.discord.exclusive_server_events(server).await else {
                        let _ = response.send(Err(DiscordError::AlreadyInChannelOnServer));
                        continue;
                    };

                    let (mut worker, handle) = CallWorker::new(
                        asterisk_channel,
                        server,
                        channel,
                        self.discord.bot_user(),
                        self.discord.message_sender(),
                        events,
                    );
                    tokio::spawn(async move {
                        worker.run().await;
                    });

                    let _ = response.send(Ok(ThreadResponse::CallPrepared { call: handle }));
                }
            }
        }
    }
}
