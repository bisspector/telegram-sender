use std::sync::Arc;

use axum::{
    extract::Path,
    http::{header::CONTENT_TYPE, Method, StatusCode},
    routing::{get, post},
    Extension, Json, Router,
};
use dashmap::{DashMap, DashSet};
use serde::Deserialize;
use tower_http::cors::{Any, CorsLayer};
use tracing::{error, info};

use crate::state::{AppState, ChatCleaningStatus, Chats};

pub async fn run(state: AppState) -> anyhow::Result<()> {
    info!("starting api server...");

    let cors = CorsLayer::new()
        .allow_headers([CONTENT_TYPE])
        // allow `GET` and `POST` when accessing the resource
        .allow_methods([Method::GET, Method::POST])
        // allow requests from any origin
        .allow_origin(Any);

    let app = Router::new()
        .route("/", get(|| async { "Hello, World!" }))
        .route("/chats", get(chats))
        .route("/status", get(status))
        .route("/deleteChat/:chat_id", get(delete_chat))
        .route("/clearChat/:chat_id", get(clear_chat))
        .route("/clearChats/", post(clear_chats))
        .route("/sendMessage/", post(send_message_to_chat))
        .layer(Extension(state))
        .layer(cors);

    axum::Server::bind(&"0.0.0.0:3030".parse().unwrap())
        .serve(app.into_make_service())
        .await?;
    Ok(())
}

async fn chats(Extension(state): Extension<AppState>) -> Json<Chats> {
    Json(state.get_chats().await.unwrap())
}

async fn status(
    Extension(state): Extension<AppState>,
) -> Json<Arc<DashMap<i64, ChatCleaningStatus>>> {
    Json(state.chats_status.clone())
}

async fn delete_chat(
    Extension(state): Extension<AppState>,
    Path(chat_id): Path<i64>,
) -> Result<(), StatusCode> {
    state.delete_chat(chat_id).await.map_err(|err| {
        error!("{err}");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

async fn clear_chat(
    Extension(state): Extension<AppState>,
    Path(chat_id): Path<i64>,
) -> Result<(), StatusCode> {
    state.delete_all_members(chat_id).await.map_err(|err| {
        error!("{err}");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

#[derive(Deserialize)]
struct ClearChatsBody {
    chats: Vec<i64>,
}

async fn clear_chats(
    Extension(state): Extension<AppState>,
    Json(payload): Json<ClearChatsBody>,
) -> Result<(), StatusCode> {
    tokio::spawn(async move { state.clear_chats(payload.chats).await });
    Ok(())
}

#[derive(Deserialize)]
struct SendMessageBody {
    chats: Vec<i64>,
    message: String,
    images: Vec<String>,
    datetime: String,
}

async fn send_message_to_chat(
    Extension(state): Extension<AppState>,
    Json(payload): Json<SendMessageBody>,
) {
    tokio::spawn(async move {
        if let Err(err) = state
            .queue_message_with_images(
                payload.chats,
                payload.message,
                payload.images,
                payload.datetime,
            )
            .await
        {
            error!("error when queuing message with images to chats {err}");
        }
    });
}
