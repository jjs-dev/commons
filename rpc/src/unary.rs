use std::{convert::Infallible, marker::PhantomData};
/// Unary side: only one `Message` is passed in this direction
pub struct Unary<M>(Infallible, PhantomData<M>);

impl<M: serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static> crate::Direction
    for Unary<M>
{
    type Tx = UnaryTx<M>;
    type Rx = UnaryRx<M>;
}

/// Unary transmitter
pub struct UnaryTx<M> {
    phantom: PhantomData<M>,
    sender: hyper::body::Sender,
}

impl<M: serde::Serialize + Send + Sync + 'static> crate::Transmit for UnaryTx<M> {
    type BatchError = SendError;

    type BatchData = M;

    type BatchFuture = futures_util::future::BoxFuture<'static, Result<(), SendError>>;

    fn from_body_sender(sender: hyper::body::Sender) -> Self {
        Self {
            phantom: PhantomData,
            sender,
        }
    }

    fn send_batch(self, uf: Self::BatchData) -> Self::BatchFuture {
        Box::pin(self.send(uf))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error("serialization failed")]
    Serialize(serde_json::Error),
    #[error("data not sent to client")]
    Network(hyper::Error),
}

impl<M: serde::Serialize> UnaryTx<M> {
    /// Sends message to the server.
    /// Consumes transmitter because multiple messages are not allowed
    /// by Unary direction.
    pub async fn send(mut self, message: M) -> Result<(), SendError> {
        let message = serde_json::to_vec(&message).map_err(SendError::Serialize)?;
        self.sender
            .send_data(message.into())
            .await
            .map_err(SendError::Network)
    }
}

/// Unary reciever
pub struct UnaryRx<M> {
    phantom: PhantomData<M>,
    body: hyper::Body,
}

impl<M: serde::de::DeserializeOwned + Send + 'static> crate::Receive for UnaryRx<M> {
    type BatchData = M;
    type BatchError = RecvError;
    type BatchFuture = futures_util::future::BoxFuture<'static, Result<M, RecvError>>;

    fn from_body(body: hyper::Body) -> Self {
        Self {
            phantom: PhantomData,
            body,
        }
    }

    fn recv_batch(self) -> Self::BatchFuture {
        Box::pin(self.recv())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RecvError {
    #[error("unable to read response")]
    Network(hyper::Error),
    #[error("parsing failed")]
    Deserialize(serde_json::Error),
}

impl<M: serde::de::DeserializeOwned> UnaryRx<M> {
    /// Receives value, passed by client.
    /// Consumes receiver because only one message could be sent.
    pub async fn recv(self) -> Result<M, RecvError> {
        let data = hyper::body::to_bytes(self.body)
            .await
            .map_err(RecvError::Network)?;
        let message = serde_json::from_slice(&*data).map_err(RecvError::Deserialize)?;
        Ok(message)
    }
}
