use anyhow::{anyhow, Result};
use chrono::Local;
use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,twilight_gateway=warn,twilight_http=warn"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();

    let cfg = common::Config::from_env()?;

    let mut rx = transport::start_gateway(cfg.token.clone()).await?;

    //transport::send_message(&cfg.token, cfg.channel_id, "bot online").await?;

    println!("pgp-disc (discord) — connected");
    println!("Channel ID: {}", cfg.channel_id);
    println!("Commands: help\n");

    let stdin = BufReader::new(io::stdin());
    let mut lines = stdin.lines();

    print_prompt(RememberedPrompt::Normal).await?;

    loop {
        tokio::select! {
            maybe = rx.recv() => {
                if let Some(ev) = maybe {
                    if ev.channel_id == cfg.channel_id {
                        render_incoming(&ev.author, &ev.content).await?;
                        print_prompt(RememberedPrompt::Normal).await?;
                    }
                }
            }

            line = lines.next_line() => {
                let line = match line? {
                    Some(l) => l,
                    None => break,
                };
                let line = line.trim();
                if line.is_empty() {
                    print_prompt(RememberedPrompt::Normal).await?;
                    continue;
                }

                match handle_command(line, &cfg).await {
                    Ok(CmdOutcome::Continue { print_prompt: p }) => {
                        if p {
                            print_prompt(RememberedPrompt::Normal).await?;
                        }
                    }
                    Ok(CmdOutcome::Quit) => {
                        println!("exiting...");
                        break;
                    }
                    Err(e) => {
                        render_error(&e.to_string()).await?;
                        print_prompt(RememberedPrompt::Normal).await?;
                    }
                }
            }

            _ = tokio::signal::ctrl_c() => {
                println!("\nexiting...");
                break;
            }
        }
    }

    Ok(())
}

enum CmdOutcome {
    Continue { print_prompt: bool },
    Quit,
}

#[derive(Copy, Clone)]
enum RememberedPrompt { Normal }

async fn handle_command(line: &str, cfg: &common::Config) -> Result<CmdOutcome> {
    let mut parts = line.split_whitespace();
    let cmd = parts.next().ok_or_else(|| anyhow!("empty command"))?;

    match cmd {
        "help" | "h" | "?" => {
            println!("Commands:");
            println!("  send <message...>   Send message to channel");
            println!("  help                Show this help");
            println!("  quit                Exit");
            Ok(CmdOutcome::Continue { print_prompt: true })
        }

        "send" | "s" => {
            let msg = parts.collect::<Vec<_>>().join(" ");
            if msg.is_empty() {
                return Err(anyhow!("Usage: send <message...>"));
            }

            transport::send_message(&cfg.token, cfg.channel_id, &msg).await?;
            render_outgoing_sent().await?;
            Ok(CmdOutcome::Continue { print_prompt: false })
        }

        "quit" | "exit" | "q" => Ok(CmdOutcome::Quit),

        _ => Err(anyhow!("Unknown command: {cmd} (try: help)")),
    }
}

async fn render_incoming(author: &str, content: &str) -> Result<()> {
    let ts = Local::now().format("%H:%M:%S");
    let mut out = io::stdout();
    out.write_all(format!("\n[{ts}] \u{2190} {author}: {content}\n").as_bytes()).await?;
    out.flush().await?;
    Ok(())
}

async fn render_outgoing_sent() -> Result<()> {
    let mut out = io::stdout();
    out.write_all("→ sent\n".as_bytes()).await?;
    out.flush().await?;
    Ok(())
}

async fn render_error(msg: &str) -> Result<()> {
    let mut out = io::stdout();
    out.write_all(format!("! error: {msg}\n").as_bytes()).await?;
    out.flush().await?;
    Ok(())
}

async fn print_prompt(_mode: RememberedPrompt) -> Result<()> {
    let mut out = io::stdout();
    out.write_all("pgp-disc> ".as_bytes()).await?;
    out.flush().await?;
    Ok(())
}
