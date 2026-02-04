use anyhow::{Result, anyhow};
use chrono::Local;
use owo_colors::OwoColorize;
use rustyline::{
    Context, Editor, Helper,
    completion::{Completer, Pair},
    error::ReadlineError,
    highlight::Highlighter,
    hint::Hinter,
    validate::Validator,
};
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Clone, Default)]
struct SessionEnv {
    default_fpr: Option<String>,
    channel_id: Option<u64>,
}

impl SessionEnv {
    fn set_channel_id(&self, cfg: &common::Config) -> u64 {
        self.channel_id.unwrap_or(cfg.channel_id)
    }
}

#[derive(Debug, Clone)]
enum UiEvent {
    Line(String),
    Clear,
    Exit,
}

#[derive(Clone)]
struct CliHelper {
    commands: Arc<Vec<&'static str>>,
    pgp_sub: Arc<Vec<&'static str>>,
    pgp_send_flags: Arc<Vec<&'static str>>,
    export_sub: Arc<Vec<&'static str>>,
    export_unset: Arc<Vec<&'static str>>,
}

impl Helper for CliHelper {}
impl Hinter for CliHelper {
    type Hint = String;
}
impl Validator for CliHelper {}
impl Highlighter for CliHelper {}

impl Completer for CliHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> std::result::Result<(usize, Vec<Pair>), ReadlineError> {
        let before = &line[..pos];
        let parts: Vec<&str> = before.split_whitespace().collect();

        let choices: &[&'static str] = match parts.as_slice() {
            [] => &self.commands,

            ["pgp"] => &self.pgp_sub,
            ["pgp", "send"] => &self.pgp_send_flags,
            ["pgp", "send", flag] if flag.starts_with('-') => &self.pgp_send_flags,
            ["pgp", _] => &self.pgp_sub,

            ["export"] => &self.export_sub,
            ["export", "unset"] => &self.export_unset,
            ["export", _] => &self.export_sub,

            _ => &self.commands,
        };

        let start = before
            .rfind(char::is_whitespace)
            .map(|i| i + 1)
            .unwrap_or(0);
        let token = &before[start..];

        let mut out = Vec::new();
        for &c in choices {
            if c.starts_with(token) {
                out.push(Pair {
                    display: c.to_string(),
                    replacement: c.to_string(),
                });
            }
        }

        Ok((start, out))
    }
}

struct UiPrinter {
    inner: Option<Box<dyn rustyline::ExternalPrinter + Send>>,
}

impl UiPrinter {
    fn new(inner: Option<Box<dyn rustyline::ExternalPrinter + Send>>) -> Self {
        Self { inner }
    }

    fn print_line(&mut self, s: &str) {
        for line in s.split('\n') {
            let mut out = line.to_string();
            out.push('\n');

            if let Some(p) = self.inner.as_mut() {
                let _ = p.print(out);
            } else {
                print!("{out}");
            }
        }
    }
}

fn spawn_cli_thread() -> (
    mpsc::UnboundedSender<UiEvent>,
    mpsc::UnboundedReceiver<String>,
) {
    let (ui_tx, ui_rx) = mpsc::unbounded_channel::<UiEvent>();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<String>();

    std::thread::spawn(move || {
        let h = CliHelper {
            commands: Arc::new(vec![
                "help", "h", "?", "me", "keys", "send", "s", "load", "pgp", "export", "quit",
                "exit", "q", "clear",
            ]),
            pgp_sub: Arc::new(vec!["list", "send", "decrypt", "decrypt-last"]),
            pgp_send_flags: Arc::new(vec!["-r"]),
            export_sub: Arc::new(vec!["recipient", "channel", "show", "unset"]),
            export_unset: Arc::new(vec!["recipient", "channel"]),
        };

        let mut rl = Editor::new().expect("rustyline editor");
        rl.set_helper(Some(h));

        let printer = rl
            .create_external_printer()
            .ok()
            .map(|p| Box::new(p) as Box<dyn rustyline::ExternalPrinter + Send>);

        let mut printer = UiPrinter::new(printer);
        std::thread::spawn(move || {
            let mut ui_rx = ui_rx;
            while let Some(ev) = ui_rx.blocking_recv() {
                match ev {
                    UiEvent::Line(s) => printer.print_line(&s),
                    UiEvent::Clear => printer.print_line("\x1B[2J\x1B[H"),
                    UiEvent::Exit => {
                        printer.print_line(&format!("{}", "exiting...".dimmed()));
                        break;
                    }
                }
            }
        });

        let hist = ".pgp-disc.history";
        let _ = rl.load_history(hist);

        loop {
            match rl.readline(&format!("{}", "pgp-disc> ".cyan())) {
                Ok(line) => {
                    let line = line.trim().to_string();
                    if line.is_empty() {
                        continue;
                    }
                    let _ = rl.add_history_entry(line.as_str());
                    if cmd_tx.send(line.clone()).is_err() {
                        break;
                    }
                    match line.as_str() {
                        "quit" | "exit" | "q" => break,
                        _ => {}
                    }
                }
                Err(ReadlineError::Interrupted) => {
                    let _ = cmd_tx.send("quit".to_string());
                    break;
                }
                Err(ReadlineError::Eof) => {
                    let _ = cmd_tx.send("quit".to_string());
                    break;
                }
                Err(_) => {
                    let _ = cmd_tx.send("quit".to_string());
                    break;
                }
            }
        }

        let _ = rl.save_history(hist);
    });

    (ui_tx, cmd_rx)
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let mut env = SessionEnv::default();

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,twilight_gateway=warn,twilight_http=warn"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();

    let cfg = common::Config::from_env()?;
    let mut rx = transport::start_gateway(cfg.token.clone()).await?;

    let mut pgp_inbox: VecDeque<(String, String)> = VecDeque::new();

    let (ui_tx, mut cmd_rx) = spawn_cli_thread();

    let _ = ui_tx.send(UiEvent::Line("discord — connected".to_string()));
    let _ = ui_tx.send(UiEvent::Line(format!(
        "Channel ID: {}",
        env.set_channel_id(&cfg)
    )));
    let _ = ui_tx.send(UiEvent::Line("Commands: help\n".to_string()));

    loop {
        tokio::select! {
            maybe = rx.recv() => {
                if let Some(ev) = maybe {
                    if ev.channel_id == env.set_channel_id(&cfg) {
                        let lines = handle_chat_event(&ev, &mut pgp_inbox).await?;
                        for s in lines {
                            let _ = ui_tx.send(UiEvent::Line(s));
                        }
                    }
                }
            }

            maybe = cmd_rx.recv() => {
                let Some(line) = maybe else { break; };

                match handle_command(&line, &cfg, &mut env, &mut pgp_inbox).await {
                    Ok((outcome, lines, ui_events)) => {
                        for s in lines {
                            let _ = ui_tx.send(UiEvent::Line(s));
                        }
                        for ev in ui_events {
                            let _ = ui_tx.send(ev);
                        }
                        if matches!(outcome, CmdOutcome::Quit) {
                            let _ = ui_tx.send(UiEvent::Exit);
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = ui_tx.send(UiEvent::Line(render_error(&e.to_string())));
                    }
                }
            }

        }
    }

    Ok(())
}

enum CmdOutcome {
    Continue,
    Quit,
}

async fn handle_command(
    line: &str,
    cfg: &common::Config,
    env: &mut SessionEnv,
    pgp_inbox: &mut VecDeque<(String, String)>,
) -> Result<(CmdOutcome, Vec<String>, Vec<UiEvent>)> {
    let mut parts = line.split_whitespace();
    let cmd = parts.next().ok_or_else(|| anyhow!("empty command"))?;

    let mut out_lines: Vec<String> = Vec::new();
    let mut ui_events: Vec<UiEvent> = Vec::new();

    match cmd {
        "export" => {
            let what = parts.next().unwrap_or("");

            match what {
                "recipient" => {
                    let v = parts.collect::<Vec<_>>().join(" ").trim().to_string();
                    if v.is_empty() {
                        return Err(anyhow!("Usage: export recipient <fpr|uid>"));
                    }
                    env.default_fpr = Some(v.clone());
                    out_lines.push(format!("{} {}", "exported recipient =".green(), v.cyan()));
                }

                "channel" => {
                    let v = parts
                        .next()
                        .ok_or_else(|| anyhow!("Usage: export channel <channel_id>"))?;
                    let ch: u64 = v
                        .parse()
                        .map_err(|_| anyhow!("channel_id must be an integer"))?;
                    env.channel_id = Some(ch);
                    out_lines.push(format!(
                        "{} {}",
                        "exported channel =".green(),
                        ch.to_string().cyan()
                    ));
                    out_lines.push(
                        "Note: now listening/sending only in this channel."
                            .dimmed()
                            .to_string(),
                    );
                }

                "show" => {
                    out_lines.push("Session exports:".bold().to_string());
                    out_lines.push(format!(
                        "  {} {}",
                        "channel".dimmed(),
                        env.set_channel_id(cfg).to_string().cyan()
                    ));
                    out_lines.push(format!(
                        "  {} {}",
                        "recipient".dimmed(),
                        env.default_fpr.as_deref().unwrap_or("(not set)").cyan()
                    ));
                }

                "unset" => {
                    let which = parts.next().unwrap_or("");
                    match which {
                        "recipient" => {
                            env.default_fpr = None;
                            out_lines.push("unset recipient".yellow().to_string());
                        }
                        "channel" => {
                            env.channel_id = None;
                            out_lines.push(format!(
                                "{} {}",
                                "unset channel (back to env)".yellow(),
                                cfg.channel_id.to_string().cyan()
                            ));
                        }
                        _ => return Err(anyhow!("Usage: export unset <fpr|channel>")),
                    }
                }

                _ => return Err(anyhow!("Usage: export <recipient|channel|show|unset> ...")),
            }

            return Ok((CmdOutcome::Continue, out_lines, ui_events));
        }
        "clear" => {
            ui_events.push(UiEvent::Clear);
            return Ok((CmdOutcome::Continue, out_lines, ui_events));
        }

        "help" | "h" | "?" => {
            out_lines.push(render_help());
            return Ok((CmdOutcome::Continue, out_lines, ui_events));
        }

        "me" => {
            if !crypto::gpg::available()? {
                out_lines.push(render_warn("gpg not found."));
                return Ok((CmdOutcome::Continue, out_lines, ui_events));
            }

            out_lines.push(crypto::gpg::version_line()?.dimmed().to_string());

            let fprs = crypto::gpg::list_secret_fingerprints()?;
            if fprs.is_empty() {
                out_lines.push(render_warn("No secret keys found in your GPG keyring."));
            } else {
                out_lines.push("Secret key fingerprints:".bold().to_string());
                for f in fprs {
                    out_lines.push(format!("  {}", f.dimmed()));
                }
            }

            return Ok((CmdOutcome::Continue, out_lines, ui_events));
        }

        "send" | "s" => {
            let msg = parts.collect::<Vec<_>>().join(" ");
            if msg.is_empty() {
                return Err(anyhow!("Usage: send <message...>"));
            }

            transport::send_message(&cfg.token, env.set_channel_id(&cfg), &msg).await?;
            out_lines.push(render_outgoing_sent());
            return Ok((CmdOutcome::Continue, out_lines, ui_events));
        }

        "keys" => {
            let keys = crypto::gpg::list_public_keys()?;
            if keys.is_empty() {
                out_lines.push(render_warn("No public keys found in your GPG keyring."));
            } else {
                out_lines.push("Public keys (recipients):".bold().to_string());
                for k in keys {
                    match k.uid {
                        Some(uid) => out_lines.push(format!("  {}  —  {}", k.fpr.dimmed(), uid)),
                        _ => out_lines.push(format!("  {}", k.fpr.dimmed())),
                    }
                }
            }
            return Ok((CmdOutcome::Continue, out_lines, ui_events));
        }

        "load" => {
            let n_str = parts.next().ok_or_else(|| anyhow!("Usage: load <count>"))?;
            let n: usize = n_str
                .parse()
                .map_err(|_| anyhow!("load <count> must be a number"))?;

            let history =
                transport::fetch_messages(&cfg.token, env.set_channel_id(&cfg), n).await?;
            if history.is_empty() {
                out_lines.push(render_warn("No messages returned."));
                return Ok((CmdOutcome::Continue, out_lines, ui_events));
            }

            out_lines.push(format!(
                "{} {}/{} {}",
                "Loading".bold(),
                history.len().to_string().cyan(),
                n.to_string().cyan(),
                "messages...".bold()
            ));

            for ev in history {
                let lines = handle_chat_event(&ev, pgp_inbox).await?;
                out_lines.extend(lines);
            }

            return Ok((CmdOutcome::Continue, out_lines, ui_events));
        }

        "pgp" => {
            let sub = parts.next().unwrap_or("");
            match sub {
                "list" => {
                    if pgp_inbox.is_empty() {
                        out_lines.push(render_warn("No PGP messages captured yet."));
                    } else {
                        out_lines.push("Captured PGP messages (latest last):".bold().to_string());
                        for (id, block) in pgp_inbox.iter() {
                            out_lines.push(format!(
                                "  {} {} {}",
                                "id=".dimmed(),
                                id.purple(),
                                format!("({} chars)", block.len()).dimmed()
                            ));
                        }
                    }
                    return Ok((CmdOutcome::Continue, out_lines, ui_events));
                }

                "decrypt-last" => {
                    let Some((id, block)) = pgp_inbox.back().cloned() else {
                        out_lines.push(render_warn("No PGP messages captured yet."));
                        return Ok((CmdOutcome::Continue, out_lines, ui_events));
                    };

                    out_lines.extend(render_decrypt_attempt(&id, &block));
                    return Ok((CmdOutcome::Continue, out_lines, ui_events));
                }

                "decrypt" => {
                    let id = parts
                        .next()
                        .ok_or_else(|| anyhow!("Usage: pgp decrypt <id>"))?;
                    let Some((_id, block)) = pgp_inbox.iter().find(|(i, _)| i == id).cloned()
                    else {
                        return Err(anyhow!("No captured PGP message with id={id}"));
                    };

                    out_lines.extend(render_decrypt_attempt(id, &block));
                    return Ok((CmdOutcome::Continue, out_lines, ui_events));
                }

                "send" => {
                    let first = parts.next().ok_or_else(|| {
                        anyhow!(
                            "Usage: pgp send <message...> OR pgp send -r <fpr|uid> <message...>"
                        )
                    })?;

                    let mut recipient: Option<String> = None;
                    let mut msg_parts: Vec<String> = Vec::new();

                    if matches!(first, "-r") {
                        let r = parts
                            .next()
                            .ok_or_else(|| anyhow!("Usage: pgp send -r <fpr|uid> <message...>"))?;
                        recipient = Some(r.to_string());
                        msg_parts = parts.map(|s| s.to_string()).collect();
                        if msg_parts.is_empty() {
                            return Err(anyhow!("Usage: pgp send -r <fpr|uid> <message...>"));
                        }
                    } else {
                        // No recipient flag
                        msg_parts.push(first.to_string());
                        msg_parts.extend(parts.map(|s| s.to_string()));
                    }

                    let recipient = match recipient {
                        Some(r) => r,
                        _ => env.default_fpr.clone().ok_or_else(|| {
                            anyhow!("No exported recipient set. Use: export recipient <fpr|uid>")
                        })?,
                    };

                    let msg = msg_parts.join(" ");

                    let armored = crypto::gpg::encrypt_to_recipient(&recipient, &msg)?;
                    transport::send_message(&cfg.token, env.set_channel_id(cfg), &armored).await?;

                    out_lines.push(format!(
                        "{} {} {}",
                        "→ sent encrypted PGP message".green(),
                        "to".dimmed(),
                        recipient.cyan()
                    ));

                    return Ok((CmdOutcome::Continue, out_lines, ui_events));
                }

                _ => return Err(anyhow!("Usage: pgp <list|send|decrypt <id>|decrypt-last>")),
            }
        }

        "quit" | "exit" | "q" => {
            return Ok((CmdOutcome::Quit, out_lines, ui_events));
        }

        _ => return Err(anyhow!("Unknown command: {cmd} (try: help)")),
    }
}

async fn handle_chat_event(
    ev: &transport::ChatEvent,
    pgp_inbox: &mut VecDeque<(String, String)>,
) -> Result<Vec<String>> {
    let mut lines = Vec::new();

    if let Some((id, block)) = crypto::detect_pgp(&ev.content) {
        pgp_inbox.push_back((id.clone(), block.clone()));
        while pgp_inbox.len() > 50 {
            pgp_inbox.pop_front();
        }

        match crypto::gpg::decrypt(&block) {
            Ok(pt) => lines.push(render_pgp_decrypted(&ev.author, &id, &pt)),
            Err(crypto::gpg::DecryptError::NotForMe { .. }) => {
                lines.push(render_pgp_unknown(&ev.author, &id))
            }
            Err(crypto::gpg::DecryptError::InvalidMessage { .. }) => {
                lines.push(render_pgp_invalid(&ev.author, &id))
            }
            Err(e) => {
                lines.push(render_pgp_error(&ev.author, &id));
                tracing::debug!("{e:?}");
            }
        }
    } else {
        lines.push(render_incoming(&ev.author, &ev.content));
    }

    Ok(lines)
}

fn render_help() -> String {
    fn section(s: &mut String, title: &str) {
        s.push_str(&format!("\n{}\n", title.bold()));
    }

    let core: &[(&str, &str)] = &[
        ("help | h | ?", "Show this help"),
        ("me", "Show your local GPG secret key fingerprints"),
        (
            "keys",
            "List public keys (recipients) from your GPG keyring",
        ),
        (
            "load <count>",
            "Load and replay last <count> messages from the channel",
        ),
        (
            "send <message...> | s <message...>",
            "Send message to channel",
        ),
        ("clear", "Clear the screen"),
        ("quit | exit | q", "Exit"),
    ];

    let pgp: &[(&str, &str)] = &[
        ("pgp list", "List captured PGP blocks"),
        ("pgp decrypt <id>", "Try to decrypt a captured PGP block"),
        (
            "pgp decrypt-last",
            "Try to decrypt the latest captured PGP block",
        ),
        (
            "pgp send <message...>",
            "Encrypt and send using exported recipient",
        ),
        (
            "pgp send -r <fpr|uid> <message...>",
            "Encrypt and send to an explicit recipient",
        ),
    ];

    let exports: &[(&str, &str)] = &[
        (
            "export recipient <fpr|uid>",
            "Set default PGP recipient for this session",
        ),
        (
            "export channel <id>",
            "Override Discord channel for send/listen",
        ),
        ("export show", "Show current exported session values"),
        ("export unset <recipient|channel>", "Clear exported value"),
    ];

    let all = core.iter().chain(pgp.iter()).chain(exports.iter());
    let max_cmd_len = all.clone().map(|(c, _)| c.len()).max().unwrap_or(0);

    let col_width = max_cmd_len + 4;

    let pad = |cmd: &str| format!("{cmd:<width$}", width = col_width);

    let mut s = String::new();

    section(&mut s, "Commands:");
    for (cmd, desc) in core {
        s.push_str(&format!("  {} {}\n", pad(cmd).cyan(), desc.dimmed()));
    }

    section(&mut s, "PGP:");
    for (cmd, desc) in pgp {
        s.push_str(&format!("  {} {}\n", pad(cmd).cyan(), desc.dimmed()));
    }

    section(&mut s, "Session exports (live only):");
    for (cmd, desc) in exports {
        s.push_str(&format!("  {} {}\n", pad(cmd).cyan(), desc.dimmed()));
    }

    s
}

fn render_warn(msg: &str) -> String {
    format!("{}", msg.yellow())
}

fn render_error(msg: &str) -> String {
    format!("{} {}", "!".red().bold(), msg.red())
}

fn ts() -> String {
    Local::now().format("%H:%M:%S").to_string()
}

fn render_incoming(author: &str, content: &str) -> String {
    format!(
        "\n[{}] {} {}: {}",
        ts().dimmed(),
        "←".cyan(),
        author.cyan(),
        content
    )
}

fn render_outgoing_sent() -> String {
    "→ sent".green().to_string()
}

fn render_decrypt_attempt(id: &str, block: &str) -> Vec<String> {
    let mut out = Vec::new();
    match crypto::gpg::decrypt(block) {
        Ok(pt) => {
            out.push(format!(
                "{} {}",
                "Decrypted".green().bold(),
                format!("(id={id})").dimmed()
            ));
            out.push(pt);
        }
        Err(crypto::gpg::DecryptError::NotForMe { .. }) => {
            out.push(format!(
                "{} {}",
                "Not for me".yellow(),
                format!("(id={id})").dimmed()
            ));
        }
        Err(crypto::gpg::DecryptError::InvalidMessage { .. }) => {
            out.push(format!(
                "{} {}",
                "Invalid PGP message".red(),
                format!("(id={id})").dimmed()
            ));
        }
        Err(e) => {
            out.push(format!(
                "{} {}",
                "Decrypt error".red(),
                format!("(id={id})").dimmed()
            ));
            tracing::debug!("{e:?}");
        }
    }
    out
}

fn render_pgp_decrypted(author: &str, id: &str, plaintext: &str) -> String {
    format!(
        "\n[{}] {} {}: {} {} {} \n{}",
        ts().dimmed(),
        "←".cyan(),
        author.cyan(),
        "[PGP]".purple(),
        format!("id={id}").dimmed(),
        "decrypted".green(),
        plaintext.green()
    )
}

fn render_pgp_invalid(author: &str, id: &str) -> String {
    format!(
        "\n[{}] {} {}: {} {} {}",
        ts().dimmed(),
        "←".cyan(),
        author.cyan(),
        "[PGP]".purple(),
        format!("id={id}").dimmed(),
        "invalid".red()
    )
}

fn render_pgp_error(author: &str, id: &str) -> String {
    format!(
        "\n[{}] {} {}: {} {} {}",
        ts().dimmed(),
        "←".cyan(),
        author.cyan(),
        "[PGP]".purple(),
        format!("id={id}").dimmed(),
        "decrypt error".red()
    )
}

fn render_pgp_unknown(author: &str, id: &str) -> String {
    format!(
        "\n[{}] {} {}: {} {} {}",
        ts().dimmed(),
        "←".cyan(),
        author.cyan(),
        "[PGP]".purple(),
        format!("id={id}").dimmed(),
        "not for me".yellow()
    )
}
