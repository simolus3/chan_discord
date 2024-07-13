use thiserror::Error;

pub type ChanRes<T> = Result<T, DiscordError>;

#[derive(Error, Debug)]
pub enum DiscordError {
    #[error("Invalid discord credentials")]
    InvalidCredentials,
    #[error("Internal error occurred")]
    InternalError {
        #[source]
        source: anyhow::Error,
    },
    #[error("The bot is already in a channel on the requested server")]
    AlreadyInChannelOnServer,
    #[error("Could not encode data to opus")]
    EncodeError,
}
