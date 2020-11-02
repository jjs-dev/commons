use hyper::service::Service;
use std::{
    convert::TryInto,
    fmt::{self, Debug, Display, Formatter},
};

type HttpRequest = hyper::Request<hyper::Body>;
type HttpResponse = hyper::Response<hyper::Body>;

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

/// Possible errors when using Reqwest-based Engine
#[derive(Debug, thiserror::Error)]
pub enum ReqwestError {
    #[error("transport error")]
    Reqwest(#[from] reqwest::Error),
    #[error("error from http crate")]
    Http(#[from] hyper::http::Error),
}

impl Service<HttpRequest> for ReqwestEngine {
    type Response = HttpResponse;
    type Error = ReqwestError;
    type Future = futures_util::future::BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, hyper_req: HttpRequest) -> Self::Future {
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

/// Type-erased engine
pub type BoxEngine = tower_util::BoxService<HttpRequest, HttpResponse, BoxEngineError>;

pub struct BoxEngineError(anyhow::Error);

impl BoxEngineError {
    fn new<E: std::error::Error + Send + Sync + 'static>(e: E) -> Self {
        Self(anyhow::Error::new(e))
    }
}

impl Debug for BoxEngineError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Debug::fmt(&self.0, f)
    }
}

impl Display for BoxEngineError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.0, f)
    }
}

impl std::error::Error for BoxEngineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0.source()
    }
}

/// Type-erases provided engine
pub fn box_engine<E>(engine: E) -> BoxEngine
where
    E: Service<HttpRequest, Response = HttpResponse> + Send + 'static,
    E::Error: std::error::Error + Send + Sync + 'static,
    E::Future: Send + 'static,
{
    struct S<E>(E);

    impl<E> Service<HttpRequest> for S<E>
    where
        E: Service<HttpRequest, Response = HttpResponse>,
        E::Error: std::error::Error + Send + Sync + 'static,
        E::Future: Send + 'static,
    {
        type Response = HttpResponse;
        type Error = BoxEngineError;
        type Future = futures_util::future::BoxFuture<'static, Result<Self::Response, Self::Error>>;

        fn poll_ready(
            &mut self,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            self.0.poll_ready(cx).map_err(BoxEngineError::new)
        }

        fn call(&mut self, req: HttpRequest) -> Self::Future {
            let inner_fut = self.0.call(req);
            Box::pin(async move { inner_fut.await.map_err(BoxEngineError::new) })
        }
    }

    BoxEngine::new(S(engine))
}
