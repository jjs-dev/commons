use std::io::Write;
use tokio::io::AsyncBufReadExt;

#[tokio::main]
async fn main() {
    tracing_subscriber::FmtSubscriber::builder()
        .with_env_filter("info,puller=trace,cli=trace")
        .init();

    let tempdir = tempfile::tempdir().expect("failed to get a tempdir");
    let image_name = match std::env::args().nth(1) {
        Some(name) => name,
        None => {
            eprintln!("Usage: batch <image_name>");
            std::process::exit(1);
        }
    };
    println!("Pulling {} to {}", &image_name, tempdir.path().display());
    let puller = puller::Puller::new().await;
    if let Err(err) = puller
        .pull(
            &image_name,
            tempdir.path(),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
    {
        eprintln!("Pull failed");
        let mut err: &(dyn std::error::Error + 'static) = &err;
        loop {
            eprintln!("{}", err);
            err = match err.source() {
                Some(e) => e,
                None => break,
            }
        }
        std::process::exit(1);
    }
    println!("OK");
    print!("(press any key to continue)");
    std::io::stdout().flush().unwrap();
    let mut line = String::new();
    let mut rdr = tokio::io::BufReader::new(tokio::io::stdin());
    rdr.read_line(&mut line).await.unwrap();
}
