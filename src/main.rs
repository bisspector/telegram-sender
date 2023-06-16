use std::env;
use std::sync::Arc;

use anyhow::anyhow;
use dashmap::DashMap;
use dotenv::dotenv;
use sqlx::postgres::PgPoolOptions;
use teloxide::adaptors::throttle::Limits;
use teloxide::{requests::RequesterExt, Bot};
use tracing::info;
use tracing_subscriber::EnvFilter;

use crate::state::AppState;

mod api;
mod bot;
mod state;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    info!("creating postrgres pool...");
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&env::var("DATABASE_URL")?)
        .await?;
    info!("running migrations...");
    sqlx::migrate!().run(&pool).await?;

    let bot = Bot::new(env::var("BOT_TOKEN")?).throttle(Limits::default());

    let state = AppState {
        pool,
        bot,
        chats_status: Arc::new(DashMap::new()),
    };

    state.fill_status_list().await?;

    match tokio::try_join!(
        tokio::spawn(bot::run(state.clone())),
        tokio::spawn(api::run(state.clone())),
        tokio::spawn(state::AppState::message_queue(state.clone())),
        tokio::spawn(state::AppState::cleanup_deprecated_chats(state.clone()))
    )? {
        (Ok(()), Ok(()), Ok(()), Ok(())) => Ok(()),
        error => Err(anyhow!("{:?}", error)),
    }
}
