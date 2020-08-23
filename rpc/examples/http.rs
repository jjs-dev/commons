//! This examples shows how to expose RPC over standard HTTP+TCP.
//! # Running
//! Pass `serve` as argument to run the server.
//! Pass `echo <smth>` to get echo from the server.
type EchoRequest = String;

type EchoResponse = String;

struct Echo;

impl rpc::Route for Echo {
    const ENDPOINT: &'static str = "/echo";
    type Request = rpc::Unary<EchoRequest>;
    type Response = rpc::Unary<EchoResponse>;
}

#[derive(Clone)]
struct Handler;

impl rpc::Handler<Echo> for Handler {
    type Error = Box<dyn std::error::Error + Send + Sync + 'static>;
    type Fut = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), Self::Error>> + Send + Sync + 'static>,
    >;
    fn handle(self, rx: rpc::UnaryRx<EchoResponse>, tx: rpc::UnaryTx<EchoResponse>) -> Self::Fut {
        Box::pin(async move {
            let data = rx.recv().await?;
            tx.send(data).await?;
            Ok(())
        })
    }
}

async fn server_main() -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
    let mut router = rpc::RouterBuilder::new();
    router.add_route(Handler);
    let router = router.build().as_make_service();
    hyper::Server::bind(&([127u8, 0, 0, 1], 8000).into())
        .serve(router)
        .await?;
    Ok(())
}

async fn client_main(
    greeting: EchoRequest,
) -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
    let engine = rpc::ReqwestEngine::new();
    let base = "http://127.0.0.1:8000".to_string();
    let mut client = rpc::Client::new(engine, base);
    let response = client.call::<Echo>(greeting).await;
    println!("RPC result: {:?}", response);
    Ok(())
}

#[tokio::main]
async fn main() {
    let res = match std::env::args().nth(1).as_deref() {
        Some("serve") => server_main().await,
        Some("echo") => {
            let greeting = std::env::args()
                .nth(1)
                .unwrap_or_else(|| "Hello rpc".to_string());
            client_main(greeting).await
        }
        _ => {
            eprintln!("Incorrect args");
            std::process::exit(1);
        }
    };
    if let Err(err) = res {
        eprintln!("Error: {}", err);
        std::process::exit(1);
    }
}
