use futures_util::StreamExt;
use std::{convert::Infallible, marker::PhantomData, pin::Pin};
use tokio::io::AsyncBufReadExt;

/// Streaming side: zero or more `Event`s can be passed in this direction,
/// Terminated by `Finish`
pub struct Streaming<E, F>(Infallible, PhantomData<(E, F)>);

impl<E, F> crate::Direction for Streaming<E, F>
where
    E: serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static,
    F: serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static,
{
    type Tx = StreamingTx<E, F>;
    type Rx = StreamingRx<E, F>;
}

/// Bytes stream will consist of oneline-serialized `Item`s.
#[derive(serde::Serialize, serde::Deserialize)]
enum Item<E, F> {
    Event(E),
    Finish(F),
}

/// Streaming transmitter
pub struct StreamingTx<E, F> {
    phantom: PhantomData<(E, F)>,
    sender: hyper::body::Sender,
}

impl<E, F> crate::Transmit for StreamingTx<E, F>
where
    E: serde::Serialize + Send + Sync + 'static,
    F: serde::Serialize + Send + Sync + 'static,
{
    type BatchError = SendError;

    type BatchData = (futures_util::stream::BoxStream<'static, E>, F);

    type BatchFuture = futures_util::future::BoxFuture<'static, Result<(), SendError>>;

    fn from_body_sender(sender: hyper::body::Sender) -> Self {
        Self {
            phantom: PhantomData,
            sender,
        }
    }

    fn send_batch(mut self, mut batch: Self::BatchData) -> Self::BatchFuture {
        Box::pin(async move {
            while let Some(event) = batch.0.next().await {
                self.send_event(event).await?;
            }

            self.finish(batch.1).await
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error("serialization failed")]
    Serialize(serde_json::Error),
    #[error("data not sent to client")]
    Network(hyper::Error),
}

impl<E: serde::Serialize, F: serde::Serialize> StreamingTx<E, F> {
    async fn priv_send(&mut self, item: Item<E, F>) -> Result<(), SendError> {
        let mut message = serde_json::to_vec(&item).map_err(SendError::Serialize)?;
        message.push(b'\n');
        self.sender
            .send_data(message.into())
            .await
            .map_err(SendError::Network)
    }

    /// Sends event.
    pub async fn send_event(&mut self, event: E) -> Result<(), SendError> {
        self.priv_send(Item::Event(event)).await
    }
    /// Sends final message.
    /// Consumes transmitter because further messages are not allowed.
    pub async fn finish(&mut self, finish: F) -> Result<(), SendError> {
        self.priv_send(Item::Finish(finish)).await
    }
}

impl<E, F> StreamingTx<E, F> {}

/// Streaming receiver
pub struct StreamingRx<E, F> {
    phantom: PhantomData<(E, F)>,
    body: tokio::io::BufReader<Pin<Box<dyn tokio::io::AsyncRead + Send + Sync + 'static>>>,
    finish: Option<F>,
}

impl<E, F> crate::Receive for StreamingRx<E, F>
where
    E: serde::de::DeserializeOwned + Send + 'static,
    F: serde::de::DeserializeOwned + Send + 'static,
{
    type BatchData = (Vec<E>, F);
    type BatchError = RecvError;
    type BatchFuture = futures_util::future::BoxFuture<'static, Result<Self::BatchData, RecvError>>;

    fn from_body(body: hyper::Body) -> Self {
        let body = body.map(|chunk| {
            chunk.map_err(|hyper_err| std::io::Error::new(std::io::ErrorKind::Other, hyper_err))
        });
        Self {
            phantom: PhantomData,
            body: tokio::io::BufReader::new(Box::pin(tokio::io::stream_reader(body))),
            finish: None,
        }
    }

    fn recv_batch(mut self) -> Self::BatchFuture {
        Box::pin(async move {
            let mut events = Vec::new();
            loop {
                let item = self.recv_next_item().await?;
                match item {
                    Item::Event(ev) => events.push(ev),
                    Item::Finish(fin) => break Ok((events, fin)),
                }
            }
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RecvError {
    #[error("unable to read response")]
    Network(hyper::Error),
    #[error("io error")]
    Io(std::io::Error),
    #[error("parsing failed")]
    Deserialize(serde_json::Error),
    #[error("unexpected EOF")]
    UnexpectedEof,
}

impl<E: serde::de::DeserializeOwned, F: serde::de::DeserializeOwned> StreamingRx<E, F> {
    async fn recv_next_item(&mut self) -> Result<Item<E, F>, RecvError> {
        let mut line = String::new();
        self.body
            .read_line(&mut line)
            .await
            .map_err(RecvError::Io)?;
        Ok(serde_json::from_str(line.trim()).map_err(RecvError::Deserialize)?)
    }
    /// Receives and returns next event.
    /// If next message in fact is Finish, returns None.
    /// In that case, use `finish` method to receive it.
    pub async fn next_event(&mut self) -> Result<Option<E>, RecvError> {
        if self.finish.is_some() {
            // provide fuse semantics
            return Ok(None);
        }
        match self.recv_next_item().await? {
            Item::Event(ev) => Ok(Some(ev)),
            Item::Finish(fin) => {
                self.finish = Some(fin);
                Ok(None)
            }
        }
    }
}
