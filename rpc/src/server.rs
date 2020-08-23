use crate::{Direction, Receive, Transmit};
use std::future::Future;
use std::sync::Arc;

/// Type that can handle requests for specific route.
/// `C` is context type.
pub trait Handler<R: crate::Route>: Clone + Send + Sync + 'static {
    /// Fatal error that can occur during processing request.
    /// this error will be logged, and response will be aborted.
    type Error: std::fmt::Display + Send + 'static;
    type Fut: Future<Output = Result<(), Self::Error>> + Send + 'static;
    fn handle(
        self,
        request: <R::Request as Direction>::Rx,
        response: <R::Response as Direction>::Tx,
    ) -> Self::Fut;
}

struct DynRoute {
    /// Route endpoint
    endpoint: &'static str,
    /// Handler for this endpoint
    func: Box<dyn Fn(hyper::Body) -> hyper::Body + Send + Sync + 'static>,
}

/// Builder for Router.
pub struct RouterBuilder {
    /// Contains DynRoutes, sorted by endpoint.
    /// Why do we sort? Because we want efficiently find handlers later,
    /// and we use binary search for it. We could use stuff like HashMap,
    /// but it performs badly on small route count.
    routes: Vec<DynRoute>,
}

impl RouterBuilder {
    /// Creates new builder with empty set of routes.
    pub fn new() -> RouterBuilder {
        RouterBuilder { routes: vec![] }
    }
    /// Adds a route to the router. Takes route handler - function that will
    /// handle requests to this route.
    /// # Panics
    /// Panics if other route with the same endpoint was added earlier.
    pub fn add_route<R: crate::Route, H: Handler<R>>(&mut self, handler: H) {
        let func = move |req: hyper::Body| {
            // create handler for this request.
            let handler = handler.clone();
            let (resp_body_sender, response_body) = hyper::Body::channel();
            let tx = <R::Response as Direction>::Tx::from_body_sender(resp_body_sender);
            let rx = <R::Request as Direction>::Rx::from_body(req);
            // start background task that will make response stream.
            tokio::task::spawn(async {
                let handler_fut = handler.handle(rx, tx);
                if let Err(err) = handler_fut.await {
                    tracing::warn!(error=%err, "request failed");
                }
            });
            response_body
        };

        let item = DynRoute {
            endpoint: R::ENDPOINT,
            func: Box::new(func),
        };
        match self
            .routes
            .binary_search_by_key(&R::ENDPOINT, |dr| dr.endpoint)
        {
            Ok(_) => panic!("duplicate endpoint {}", R::ENDPOINT),
            Err(pos) => self.routes.insert(pos, item),
        }
    }

    /// Converts this builder into service, which can be further used with hyper.
    pub fn build(self) -> Router {
        Router {
            routes: self.routes.into(),
        }
    }
}

impl Default for RouterBuilder {
    fn default() -> Self {
        RouterBuilder::new()
    }
}

/// Tower Service which can be used to serve requests.
#[derive(Clone)]
pub struct Router {
    routes: Arc<[DynRoute]>,
}

/// Wrapper around Router, implementing MakeService.
pub struct MakeRouter(Router);

impl Router {
    fn find_route(&self, req: &hyper::Request<hyper::Body>) -> Option<&DynRoute> {
        let requested_endpoint = req.uri().path();
        match self
            .routes
            .binary_search_by_key(&requested_endpoint, |dr| dr.endpoint)
        {
            Ok(pos) => Some(&self.routes[pos]),
            Err(_) => None,
        }
    }

    pub fn as_make_service(&self) -> MakeRouter {
        MakeRouter(self.clone())
    }

    fn call_inner(&self, req: hyper::Request<hyper::Body>) -> hyper::Response<hyper::Body> {
        if req.method() != hyper::Method::POST {
            return hyper::Response::builder()
                .status(hyper::StatusCode::METHOD_NOT_ALLOWED)
                .body("only POST is allowed".into())
                .unwrap();
        }
        let dyn_route = match self.find_route(&req) {
            Some(dr) => dr,
            None => {
                tracing::error!("unknown endpoint");
                return hyper::Response::builder()
                    .status(hyper::StatusCode::NOT_FOUND)
                    .body("Unknown endpoint".into())
                    .unwrap();
            }
        };
        let response_body = (dyn_route.func)(req.into_body());
        hyper::Response::new(response_body)
    }
}

impl hyper::service::Service<hyper::Request<hyper::Body>> for Router {
    type Response = hyper::Response<hyper::Body>;
    type Error = std::convert::Infallible;
    type Future = futures_util::future::Ready<Result<Self::Response, Self::Error>>;
    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }
    fn call(&mut self, req: hyper::Request<hyper::Body>) -> Self::Future {
        futures_util::future::ready(Ok(self.call_inner(req)))
    }
}

impl<T> hyper::service::Service<T> for MakeRouter {
    type Response = Router;
    type Error = std::convert::Infallible;
    type Future = futures_util::future::Ready<Result<Self::Response, Self::Error>>;
    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, _req: T) -> Self::Future {
        futures_util::future::ready(Ok(self.0.clone()))
    }
}
