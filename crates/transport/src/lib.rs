use anyhow::{anyhow, Result};
use tokio::sync::mpsc;
use tracing::{error, info};

use twilight_gateway::{Event, EventTypeFlags, Intents, Shard, ShardId, StreamExt as _};
use twilight_http::Client as HttpClient;
use twilight_model::gateway::payload::incoming::MessageCreate;
use twilight_model::id::{marker::ChannelMarker, Id};

#[derive(Clone, Debug)]
pub struct ChatEvent {
    pub channel_id: u64,
    pub author_id: u64,
    pub author: String,
    pub content: String,
}

/// Starts a Discord Gateway connection.
/// Login as discord bot using `token`.
pub async fn start_gateway(token: String) -> Result<mpsc::Receiver<ChatEvent>> {
    let intents = Intents::GUILD_MESSAGES | Intents::MESSAGE_CONTENT;

    let mut shard = Shard::new(ShardId::ONE, token, intents);

    let (tx, rx) = mpsc::channel::<ChatEvent>(1000);

    tokio::spawn(async move {
        info!("Gateway task started");

        while let Some(item) = shard.next_event(EventTypeFlags::all()).await {
            let event = match item {
                Ok(ev) => ev,
                Err(e) => {
                    error!("Gateway receive error: {e}");
                    continue;
                }
            };

            if let Event::MessageCreate(msg) = event {
                let ev = convert_message_create(*msg);
                let _ = tx.send(ev).await;
            }
        }

        info!("Gateway task ended");
    });

    Ok(rx)
}

fn convert_message_create(msg: MessageCreate) -> ChatEvent {
    ChatEvent {
        channel_id: msg.channel_id.get(),
        author_id: msg.author.id.get(),
        author: msg.author.name.clone(),
        content: msg.content.clone(),
    }
}

/// Send a message to a channel using the Discord REST API.
pub async fn send_message(token: &str, channel_id: u64, content: &str) -> Result<()> {
    let http = HttpClient::new(token.to_string());

    let channel_id: Id<ChannelMarker> = Id::new(channel_id);

    http.create_message(channel_id)
        .content(content)
        .await
        .map_err(|e| anyhow!("Discord HTTP error: {e}"))?;

    Ok(())
}
