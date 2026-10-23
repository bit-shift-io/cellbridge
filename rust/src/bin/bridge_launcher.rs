mod common;

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args == ["--help"] {
        println!(
            "bridge_launcher --config PATH\nConfig: camelCase CellBridge JSON. Enter quit or Ctrl-C to stop."
        );
        return Ok(());
    }
    if args.len() != 2 || args[0] != "--config" {
        return Err("usage: bridge_launcher --config PATH".into());
    }
    let text = std::fs::read_to_string(&args[1]).map_err(|_| "cannot read configuration file")?;
    let cfg = serde_json::from_str(&text).map_err(|_| "invalid configuration JSON")?;
    tokio::task::spawn_blocking(move || cellbridge::native::start(cfg, None))
        .await
        .map_err(|_| "bridge worker failed")??;
    println!("Bridge started; waiting for peer. Enter quit to stop.");
    let mut lines = common::input();
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            line = lines.recv() => match line {
                Some(line) if line.trim() != "quit" => println!("{:?}", cellbridge::status().state),
                _ => break,
            }
        }
    }
    tokio::task::spawn_blocking(cellbridge::native::stop)
        .await
        .map_err(|_| "bridge worker failed")?
}
