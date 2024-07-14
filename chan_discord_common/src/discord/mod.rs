use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::sync::Arc;

use log::{debug, trace};
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;
use twilight_cache_inmemory::{InMemoryCache, ResourceType};
use twilight_gateway::{Event, Intents, MessageSender, Shard, ShardId};
use twilight_http::Client;
use twilight_model::id::marker::{GuildMarker, UserMarker};
use twilight_model::id::Id;

use crate::error::DiscordError;

pub mod crypto;
pub mod rtp;
mod voice_gateway;
pub mod voice_task;

struct DiscordInner {
    cache: InMemoryCache,
    sender: MessageSender,
    user: Id<UserMarker>,
    channels: Mutex<HashMap<Id<GuildMarker>, mpsc::Sender<Event>>>,
}

pub struct Discord {
    inner: Arc<DiscordInner>,
    cancel: CancellationToken,
}

impl Discord {
    pub async fn start(token: String) -> Result<Self, DiscordError> {
        let client = Client::new(token.clone());
        let bot_user = client
            .current_user()
            .await
            .map_err(|_| DiscordError::InvalidCredentials)?
            .model()
            .await
            .map_err(|e| DiscordError::InternalError { source: e.into() })?;
        debug!(
            "Established connection to discord! User is {} (id {})",
            bot_user.name, bot_user.id
        );
        let bot_user = bot_user.id;

        let cache = InMemoryCache::builder()
            .resource_types(ResourceType::MESSAGE)
            .build();
        let mut shard = Shard::new(
            ShardId::ONE,
            token,
            Intents::GUILD_MESSAGES | Intents::GUILD_VOICE_STATES,
        );

        let token = CancellationToken::new();
        let inner = Arc::new(DiscordInner {
            cache,
            sender: shard.sender(),
            user: bot_user,
            channels: Default::default(),
        });
        {
            let token = token.clone();
            let inner = inner.clone();
            tokio::spawn(async move {
                loop {
                    trace!("Waiting for discord global gateway event");
                    tokio::select! {
                        _ = token.cancelled() => {
                            break;
                        },
                        event = shard.next_event() => {
                            let event = match event {
                                Ok(event) => event,
                                Err(e) => {
                                    trace!("Error receiving event from global gateway: {e}");
                                    continue;
                                },
                            };
                            inner.handle_event(event).await;
                        },
                    };
                }
            });
        }
        Ok(Self {
            inner,
            cancel: token,
        })
    }

    pub fn bot_user(&self) -> Id<UserMarker> {
        self.inner.user
    }

    pub fn message_sender(&self) -> MessageSender {
        self.inner.sender.clone()
    }

    /// Returns a channel receiving events on the [server] id if no other channel is listening on
    /// that server yet.
    pub async fn exclusive_server_events(
        &self,
        server: Id<GuildMarker>,
    ) -> Option<mpsc::Receiver<Event>> {
        let mut map = self.inner.channels.lock().await;
        let (tx, rx) = mpsc::channel(32);

        match map.entry(server) {
            Entry::Occupied(mut occupied) => {
                if occupied.get().is_closed() {
                    occupied.insert(tx);
                } else {
                    return None;
                }
            }
            Entry::Vacant(empty) => {
                empty.insert(tx);
            }
        };

        Some(rx)
    }

    pub fn cancel_thread(&self) {
        self.cancel.cancel();
    }
}

impl DiscordInner {
    async fn handle_event(&self, event: Event) {
        trace!("Event on global gateway: {event:?}");

        self.cache.update(&event);
        if let Some(guild) = event.guild_id() {
            let mut lock = self.channels.lock().await;
            {
                let entry = lock.entry(guild);
                if let Entry::Occupied(entry) = entry {
                    if entry.get().send(event).await.is_err() {
                        entry.remove_entry();
                    }
                }
            }
        }
    }
}
