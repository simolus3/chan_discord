use thiserror::Error;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;

pub struct RequestSender<Req, Res> {
    sender: UnboundedSender<(Req, oneshot::Sender<Res>)>,
}

#[derive(Error, Debug)]
pub enum RequestError {
    #[error("Request dropped without response")]
    RequestDropped,
    #[error("Receiver dropped")]
    ReceiverDropped,
}

pub struct RequestReceiver<Req, Res> {
    receiver: UnboundedReceiver<(Req, oneshot::Sender<Res>)>,
}

type Request<Req, Res> = (Req, oneshot::Sender<Res>);

pub fn request_channel<Req, Res>() -> (RequestSender<Req, Res>, RequestReceiver<Req, Res>) {
    let (tx, rx) = unbounded_channel();

    (
        RequestSender { sender: tx },
        RequestReceiver { receiver: rx },
    )
}

impl<Req, Res> RequestSender<Req, Res> {
    pub async fn request(&self, request: Req) -> Result<Res, RequestError> {
        let (tx, rx) = oneshot::channel();
        if let Err(_) = self.sender.send((request, tx)) {
            return Err(RequestError::ReceiverDropped);
        }

        rx.await.map_err(|_| RequestError::ReceiverDropped)
    }

    pub fn request_blocking(&self, request: Req) -> Result<Res, RequestError> {
        let (tx, rx) = oneshot::channel();
        if let Err(_) = self.sender.send((request, tx)) {
            return Err(RequestError::ReceiverDropped);
        }

        rx.blocking_recv()
            .map_err(|_| RequestError::ReceiverDropped)
    }
}

impl<Req, Res> RequestReceiver<Req, Res> {
    pub async fn request(&mut self) -> Option<Request<Req, Res>> {
        let (req, sender) = self.receiver.recv().await?;

        Some((req, sender))
    }
}
