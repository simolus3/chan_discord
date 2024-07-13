use std::sync::mpsc;

use anyhow::anyhow;
use asterisk::{astobj2::Ao2, channel::Channel};
use asterisk_sys::bindings::{ast_control_frame_type, ast_frame};
use chan_discord_common::error::{ChanRes, DiscordError};
use log::debug;

/// A thread whose sole responsibility is to write `ast_frame`s to [Channel]s.
///
/// We can't do this in the call task or the discord thread as writing frames requires a lock on
/// the channel. The discord thread is also responsible for serving requests to the channel though,
/// so there is a potential for deadlocks in that approach, e.g. in this schedule:
///
///   1. Asterisk thread wants to write a frame to the Discord channel, obtains the lock before
///      calling `write`.
///   2. We have received a packet from Discord and want to push it into the Asterisk channel, so we
///      call `ast_queue_frame` which is blocked on obtaining a lock.
///   3. `write` is called send sends a write request to the Discord thread over an async channel.
///      Regrettably, that thread can't handle requests because it's waiting to write a frame.
///      So the write to Discord can't complete and the channel stays locked, nothing makes any
///      progress.
///
/// To fix this, we spawn a new thread whose sole responsibility is to obtain short-lived locks on
/// channels to enqueue frames. This means that the Discord thread is never waiting for a Asterisk
/// lock.
#[derive(Clone)]
pub struct QueueThread {
    sender: mpsc::Sender<ChannelWriteRequest>,
}

pub enum ChannelWriteKind {
    Hangup,
    Control {
        frame_type: ast_control_frame_type,
    },
    Frame {
        backing_memory: Vec<i16>,
        frame: ast_frame,
    },
}

impl QueueThread {
    pub fn start() -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<ChannelWriteRequest>();

        std::thread::Builder::new()
            .name("chan_discord_queue".to_string())
            .spawn(move || {
                while let Ok(request) = rx.recv() {
                    let channel = request.channel;

                    match request.write {
                        ChannelWriteKind::Hangup => {
                            channel.queue_hangup();
                        }
                        ChannelWriteKind::Control { frame_type } => {
                            channel.queue_control(frame_type);
                        }
                        ChannelWriteKind::Frame {
                            backing_memory: _,
                            mut frame,
                        } => {
                            channel.queue_frame(&mut frame);
                        }
                    }
                }

                debug!("Queue thread stopped!");
            })
            .expect("QueueThread should have started");

        Self { sender: tx }
    }

    pub fn request(&self, channel: Ao2<Channel>, write: ChannelWriteKind) -> ChanRes<()> {
        self.sender
            .send(ChannelWriteRequest { channel, write })
            .map_err(|_| DiscordError::InternalError {
                source: anyhow!("Could not reach queue thread"),
            })?;
        Ok(())
    }
}

struct ChannelWriteRequest {
    channel: Ao2<Channel>,
    write: ChannelWriteKind,
}

unsafe impl Send for ChannelWriteKind {}
