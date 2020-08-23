use std::sync::{
    atomic::{AtomicU64, Ordering::SeqCst},
    Arc,
};

// protocol
#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct AddRequest {
    a: u64,
    b: u64,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct AddResponse {
    sum: u64,
}

/// Calculate sum of two numbers and internal server counter.
struct AddRpc;

impl rpc::Route for AddRpc {
    const ENDPOINT: &'static str = "/add";
    type Request = rpc::Unary<AddRequest>;
    type Response = rpc::Unary<AddResponse>;
}

#[derive(serde::Serialize, serde::Deserialize)]
struct StreamingPiRequest {}

#[derive(serde::Serialize, serde::Deserialize)]
struct StreamingPiResponse {
    digits_chunk: String,
}
/// Get "Pi" number, with growing precision
struct StreamingPi;

impl rpc::Route for StreamingPi {
    const ENDPOINT: &'static str = "/pi";
    type Request = rpc::Unary<StreamingPiRequest>;
    type Response = rpc::Streaming<StreamingPiResponse, ()>;
}

// server
#[derive(Clone)]
struct Server {
    counter: Arc<AtomicU64>,
}

impl rpc::Handler<AddRpc> for Server {
    type Error = Box<dyn std::error::Error + Send + Sync + 'static>;
    type Fut = futures_util::future::BoxFuture<'static, Result<(), Self::Error>>;
    fn handle(self, rx: rpc::UnaryRx<AddRequest>, tx: rpc::UnaryTx<AddResponse>) -> Self::Fut {
        Box::pin(async move {
            let req = rx.recv().await?;

            let cnt = self.counter.fetch_add(1, SeqCst);

            let res = AddResponse {
                sum: req.a + req.b + cnt,
            };
            tx.send(res).await?;
            Ok(())
        })
    }
}

impl rpc::Handler<StreamingPi> for Server {
    type Error = Box<dyn std::error::Error + Send + Sync + 'static>;
    type Fut = futures_util::future::BoxFuture<'static, Result<(), Self::Error>>;
    fn handle(
        self,
        rx: rpc::UnaryRx<StreamingPiRequest>,
        mut tx: rpc::StreamingTx<StreamingPiResponse, ()>,
    ) -> Self::Fut {
        Box::pin(async move {
            let _req = rx.recv().await?;
            const CHUNKS: &[&str] = &["3.", "14", "159", "26"];
            for &chunk in CHUNKS {
                tx.send_event(StreamingPiResponse {
                    digits_chunk: chunk.to_string(),
                })
                .await?;
            }
            tx.finish(()).await?;
            Ok(())
        })
    }
}

fn make_server() -> rpc::Router {
    let mut builder = rpc::RouterBuilder::new();
    let srv = Server {
        counter: Arc::new(AtomicU64::new(0)),
    };
    builder.add_route::<AddRpc, _>(srv.clone());
    builder.add_route::<StreamingPi, _>(srv);
    builder.build()
}

#[tokio::test]
async fn test_simple() {
    let server = make_server();
    let mut client = rpc::Client::new(server, "".to_string());
    let data = AddRequest { a: 2, b: 3 };
    let resp1 = client.call::<AddRpc>(data.clone()).await.unwrap();
    assert_eq!(resp1.sum, 5);
    let resp2 = client.call::<AddRpc>(data).await.unwrap();
    assert_eq!(resp2.sum, 6);

    let pi_batch = client
        .call::<StreamingPi>(StreamingPiRequest {})
        .await
        .unwrap();
    let mut pi = String::new();
    for item in pi_batch.0 {
        pi.push_str(&item.digits_chunk);
    }
    assert_eq!(pi, "3.1415926")
}
