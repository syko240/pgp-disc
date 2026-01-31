use anyhow::{anyhow, Result};

#[derive(Clone, Debug)]
pub struct Config {
    pub token: String,
    pub channel_id: u64,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let token = std::env::var("DISCORD_TOKEN")
            .map_err(|_| anyhow!("Missing DISCORD_TOKEN env var"))?;

        let channel_id: u64 = std::env::var("DISCORD_CHANNEL_ID")
            .map_err(|_| anyhow!("Missing DISCORD_CHANNEL_ID env var"))?
            .parse()
            .map_err(|_| anyhow!("DISCORD_CHANNEL_ID must be an integer"))?;

        Ok(Self { token, channel_id })
    }
}
