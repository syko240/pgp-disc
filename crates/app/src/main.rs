use anyhow::{Result, anyhow};
use chrono::Local;
use std::collections::VecDeque;
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

    // store block in mem
    let mut pgp_inbox: VecDeque<(String, String)> = VecDeque::new();

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
                        if let Some((id, block)) = crypto::detect_pgp(&ev.content) {
                            pgp_inbox.push_back((id.clone(), block));
                            // keep last 50
                            while pgp_inbox.len() > 50 {
                                pgp_inbox.pop_front();
                            }

                            render_pgp_unknown(&ev.author, &id).await?;
                        } else {
                            render_incoming(&ev.author, &ev.content).await?;
                        }
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

                match handle_command(line, &cfg, &mut pgp_inbox).await {
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
enum RememberedPrompt {
    Normal,
}

async fn handle_command(
    line: &str,
    cfg: &common::Config,
    pgp_inbox: &mut VecDeque<(String, String)>,
) -> Result<CmdOutcome> {
    let mut parts = line.split_whitespace();
    let cmd = parts.next().ok_or_else(|| anyhow!("empty command"))?;

    match cmd {
        "me" => {
            if !crypto::gpg::available()? {
                println!("gpg not found.");
                return Ok(CmdOutcome::Continue { print_prompt: true });
            }

            println!("{}", crypto::gpg::version_line()?);

            let fprs = crypto::gpg::list_secret_fingerprints()?;
            if fprs.is_empty() {
                println!("No secret keys found in your GPG keyring.")
            } else {
                println!("Secret key fingerprints:");
                for f in fprs {
                    println!("  {f}");
                }
            }

            Ok(CmdOutcome::Continue { print_prompt: true })
        }

        "help" | "h" | "?" => {
            println!("Commands:");
            println!("  me                  Show your local GPG secret key fingerprints");
            println!("  send <message...>   Send message to channel");
            println!("  pgp list            List captured PGP blocks");
            println!("  pgp decrypt <id>    Try to decrypt a captured PGP block");
            println!("  pgp decrypt-last    Try to decrypt the latest captured PGP block");
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
            Ok(CmdOutcome::Continue {
                print_prompt: false,
            })
        }

        "pgp" => {
            let sub = parts.next().unwrap_or("");
            match sub {
                "list" => {
                    if pgp_inbox.is_empty() {
                        println!("No PGP messages captured yet.");
                    } else {
                        println!("Captured PGP messages (latest last):");
                        for (id, block) in pgp_inbox.iter() {
                            println!("  id={id} ({} chars)", block.len());
                        }
                    }
                    Ok(CmdOutcome::Continue { print_prompt: true })
                }

                "decrypt-last" => {
                    let Some((id, block)) = pgp_inbox.back().cloned() else {
                        println!("No PGP messages captured yet.");
                        return Ok(CmdOutcome::Continue { print_prompt: true });
                    };

                    match crypto::gpg::decrypt(&block) {
                        Ok(pt) => {
                            println!("Decrypted (id={id}):\n{pt}");
                        }
                        Err(e) => {
                            // TODO
                            println!("(id={id}) not for me / decrypt failed");
                            tracing::debug!("{e}");
                        }
                    }

                    Ok(CmdOutcome::Continue { print_prompt: true })
                }

                "decrypt" => {
                    let id = parts
                        .next()
                        .ok_or_else(|| anyhow!("Usage: pgp decrypt <id>"))?;
                    let Some((_id, block)) = pgp_inbox.iter().find(|(i, _)| i == id).cloned()
                    else {
                        return Err(anyhow!("No captured PGP message with id={id}"));
                    };

                    match crypto::gpg::decrypt(&block) {
                        Ok(pt) => {
                            println!("Decrypted (id={id}):\n{pt}");
                        }
                        Err(e) => {
                            println!("(id={id}) not for me / decrypt failed");
                            tracing::debug!("{e}");
                        }
                    }

                    Ok(CmdOutcome::Continue { print_prompt: true })
                }

                _ => Err(anyhow!("Usage: pgp <list|decrypt <id>|decrypt-last>")),
            }
        }

        "quit" | "exit" | "q" => Ok(CmdOutcome::Quit),

        _ => Err(anyhow!("Unknown command: {cmd} (try: help)")),
    }
}

async fn render_incoming(author: &str, content: &str) -> Result<()> {
    let ts = Local::now().format("%H:%M:%S");
    let mut out = io::stdout();
    out.write_all(format!("\n[{ts}] \u{2190} {author}: {content}\n").as_bytes())
        .await?;
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
    out.write_all(format!("! error: {msg}\n").as_bytes())
        .await?;
    out.flush().await?;
    Ok(())
}

async fn print_prompt(_mode: RememberedPrompt) -> Result<()> {
    let mut out = io::stdout();
    out.write_all("pgp-disc> ".as_bytes()).await?;
    out.flush().await?;
    Ok(())
}

async fn render_pgp_unknown(author: &str, id: &str) -> Result<()> {
    let ts = Local::now().format("%H:%M:%S");
    let mut out = io::stdout();
    out.write_all(
        format!("\n[{ts}] \u{2190} {author}: [PGP] message id={id} (unknown)\n").as_bytes(),
    )
    .await?;
    out.flush().await?;
    Ok(())
}
