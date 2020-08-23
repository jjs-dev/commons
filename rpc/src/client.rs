use crate::{Receive, Transmit};
use std::convert::TryInto;
use tower_util::ServiceExt;

/// RPC Client. `Engine` is something that can actually send requests to the
/// RPC server, e.g. hyper::Client or reqwest::Client.
#[derive(Clone)]
pub struct Client<Engine = ReqwestEngine> {
    engine: Engine,
    base: String,
}

impl<E> Client<E> {
    /// Constructs new client from the given engine and base url.
    /// Base usually should not end with '/'.
    /// All requests will be sent to "{self.base}{R::ENDPOINT}".
    pub fn new(engine: E, base: String) -> Self {
        Client { engine, base }
    }
}

#[derive(Debug)]
pub enum CallError<TransportError, RecvBatchError, SendBatchError> {
    Transport(TransportError),
    Recv(RecvBatchError),
    Send(SendBatchError),
}

impl<TE, RE, SE> CallError<TE, RE, SE> {
    pub fn description(&self) -> &'static str {
        match self {
            CallError::Transport(_) => "transport error",
            CallError::Send(_) => "failed to send batch",
            CallError::Recv(_) => "failed to receive batch",
        }
    }
}

impl<TE, RE, SE> std::fmt::Display for CallError<TE, RE, SE> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.description().fmt(f)
    }
}

impl<
        TE: std::error::Error + 'static,
        RE: std::error::Error + 'static,
        SE: std::error::Error + 'static,
    > std::error::Error for CallError<TE, RE, SE>
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CallError::Transport(inner) => Some(inner),
            CallError::Recv(inner) => Some(inner),
            CallError::Send(inner) => Some(inner),
        }
    }
}

impl<E> Client<E>
where
    E: hyper::service::Service<
        hyper::Request<hyper::Body>,
        Response = hyper::Response<hyper::Body>,
    >,
{
    /// Executes RPC call, writing all Request-sided messages upfront and
    /// returning batch of server responses.
    pub async fn call<R: crate::Route>(
        &mut self,
        data: <<R::Request as crate::Direction>::Tx as crate::Transmit>::BatchData,
    ) -> Result<
        <<R::Response as crate::Direction>::Rx as crate::Receive>::BatchData,
        CallError<
            <E as hyper::service::Service<hyper::Request<hyper::Body>>>::Error,
            <<R::Response as crate::Direction>::Rx as crate::Receive>::BatchError,
            <<R::Request as crate::Direction>::Tx as crate::Transmit>::BatchError,
        >,
    > {
        let (tx, rx) = self.start::<R>().await.map_err(CallError::Transport)?;
        tx.send_batch(data).await.map_err(CallError::Send)?;
        rx.recv_batch().await.map_err(CallError::Recv)
    }

    /// Starts new RPC call.
    /// Returns transmitter that can send messages to server,
    /// and receiver that can receive messages from server.
    pub async fn start<R: crate::Route>(
        &mut self,
    ) -> Result<
        (
            <R::Request as crate::Direction>::Tx,
            <R::Response as crate::Direction>::Rx,
        ),
        <E as hyper::service::Service<hyper::Request<hyper::Body>>>::Error,
    > {
        let (body_sender, body) = hyper::Body::channel();
        let req = hyper::Request::builder()
            .method(hyper::Method::POST)
            .uri(format!("{}{}", self.base, R::ENDPOINT))
            .body(body)
            .expect("invalid data");

        let tx = <R::Request as crate::Direction>::Tx::from_body_sender(body_sender);
        let response = (&mut self.engine).oneshot(req).await?;
        let rx = <R::Response as crate::Direction>::Rx::from_body(response.into_body());
        Ok((tx, rx))
    }
}

/// Engine based on reqwest.
#[derive(Clone)]
pub struct ReqwestEngine(reqwest::Client);

impl ReqwestEngine {
    pub fn wrap_client(cl: reqwest::Client) -> ReqwestEngine {
        ReqwestEngine(cl)
    }

    pub fn new() -> ReqwestEngine {
        ReqwestEngine(reqwest::Client::new())
    }
}

impl Default for ReqwestEngine {
    fn default() -> Self {
        ReqwestEngine::new()
    }
}

impl hyper::service::Service<hyper::Request<hyper::Body>> for ReqwestEngine {
    type Response = hyper::Response<hyper::Body>;
    type Error = Box<dyn std::error::Error + Send + Sync + 'static>;
    type Future = futures_util::future::BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, hyper_req: hyper::Request<hyper::Body>) -> Self::Future {
        let client = self.0.clone();
        Box::pin(async move {
            let reqwest_req: reqwest::Request =
                hyper_req.map(reqwest::Body::wrap_stream).try_into()?;
            let response = client.execute(reqwest_req).await?;
            let mut builder = hyper::Response::builder().status(response.status());
            for (k, v) in response.headers() {
                builder = builder.header(k, v);
            }
            builder
                .body(hyper::Body::wrap_stream(response.bytes_stream()))
                .map_err(Into::into)
        })
    }
}
