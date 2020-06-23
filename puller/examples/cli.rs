use std::io::Write;
use tokio::io::AsyncBufReadExt;

#[tracing::instrument]
async fn read_image_pull_secrets() -> Vec<puller::ImagePullSecret> {
    let docker_config_path = home::home_dir().unwrap().join(".docker/config.json");
    let docker_config = tokio::fs::read(&docker_config_path)
        .await
        .expect("docker config not found");
    let docker_config = serde_json::from_slice(&docker_config).unwrap();
    match puller::ImagePullSecret::parse_docker_config(&docker_config) {
        Some(secs) => {
            for sec in &secs {
                println!("found secret: {}", sec);
            }
            secs
        }
        None => {
            eprintln!("Warning: parsing ~/.docker/config.json failed");
            Vec::new()
        }
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::FmtSubscriber::builder()
        .with_env_filter("warn,puller=trace,cli=trace")
        .init();
    let tempdir = tempfile::tempdir().expect("failed to get a tempdir");
    let mut tokens: Vec<tokio::sync::CancellationToken> = Vec::new();
    let mut p = puller::Puller::new().await;
    let mut stdin_reader = tokio::io::BufReader::new(tokio::io::stdin());
    println!("Reading image pull secrets");
    let secrets = read_image_pull_secrets().await;
    p.set_secrets(secrets);
    loop {
        print!("> ");
        std::io::stdout().flush().unwrap();
        let mut line = String::new();
        stdin_reader.read_line(&mut line).await.unwrap();
        let line = line.trim();
        if line == "exit" {
            println!("exiting");
            break;
        }
        if line == "cancel" {
            while let Some(token) = tokens.pop() {
                token.cancel();
            }
            continue;
        }
        let token = tokio::sync::CancellationToken::new();
        tokens.push(token.clone());

        let sanitized_name = line.replace('/', "_").replace(':', "-");
        let pull_dest = tempdir.path().join(&sanitized_name);
        println!(
            "Starting new pull: image={}, dest={}",
            &line,
            pull_dest.display()
        );
        let chan = match p.pull(line, &pull_dest, token).await {
            Ok(ch) => ch,
            Err(err) => {
                eprintln!("Failed to start pulling: {}", err);
                continue;
            }
        };
        let line = line.to_string();

        tokio::task::spawn(async move {
            match chan.await.unwrap() {
                Ok(()) => {
                    println!("{}: pull succeeded", line);
                }
                Err(err) => {
                    println!("{}: pull failed: {}", line, err);
                }
            }
        });
    }
}
