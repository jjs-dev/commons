//! Simple HTTP+JSON based RPC
//!
//! `rpc` used client-server model:
//! server exposes routes, and client calls them.
//! Each route is composed from request direction (client -> server)
//! and response direction (server -> client).
//! Each direction can be unary or streaming.
mod client;
mod server;
mod streaming;
mod unary;

pub use client::{
    engines::{box_engine, BoxEngine, BoxEngineError, ReqwestEngine, ReqwestError},
    Client,
};
pub use server::{Handler, MakeRouter, Router, RouterBuilder};
pub use streaming::{
    RecvError as StreamingRecvError, SendError as StreamingSendError, Streaming, StreamingRx,
    StreamingTx,
};
pub use unary::{
    RecvError as UnaryRecvError, SendError as UnarySendError, Unary, UnaryRx, UnaryTx,
};

/// One side of RPC communication.
pub trait Direction {
    /// Type which is used to read from this direction.
    type Rx: Receive;
    /// Type which is used to write into this direction.
    type Tx: Transmit;
}

/// Type that can be constructed from the http request.
pub trait Receive: Send + 'static {
    type BatchData;
    type BatchError: std::error::Error + Send + Sync + 'static;
    type BatchFuture: std::future::Future<Output = Result<Self::BatchData, Self::BatchError>>;

    fn from_body(req: hyper::Body) -> Self;

    fn recv_batch(self) -> Self::BatchFuture;
}

/// Type that can produce http request.
pub trait Transmit: Send + 'static {
    type BatchError: std::error::Error + Send + Sync + 'static;
    type BatchData;
    type BatchFuture: std::future::Future<Output = Result<(), Self::BatchError>>;

    fn from_body_sender(send: hyper::body::Sender) -> Self;

    fn send_batch(self, uf: Self::BatchData) -> Self::BatchFuture;
}

/// Single RPC call offered by the server.
pub trait Route {
    /// How client should interact with server.
    type Request: Direction;
    /// How server should respond to client.
    type Response: Direction;
    /// URL at which this endpoint should be available.
    /// `ENDPOINT` must start with `/`.
    const ENDPOINT: &'static str;
}
