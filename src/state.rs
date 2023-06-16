use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::Context;
use base64::Engine;
use bytes::Bytes;
use dashmap::DashMap;
use data_url::DataUrl;
use futures::TryStreamExt;
use serde::Serialize;
use sqlx::PgPool;
use teloxide::{
    adaptors::Throttle,
    payloads::SendMessageSetters,
    requests::{Requester, RequesterExt},
    types::{ChatId, InputFile, InputMedia, InputMediaPhoto, ParseMode, UserId},
    utils::markdown::escape,
    Bot,
};
use tracing::{error, info};

pub type WrappedBot = Throttle<Bot>;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub bot: WrappedBot,
    pub chats_status: Arc<DashMap<i64, ChatCleaningStatus>>,
}

#[derive(Serialize)]
pub enum ChatCleaningStatus {
    Idle,
    Queued,
    InProgress,
    Error(String),
}

#[derive(Clone)]
struct QueuedMessage {
    id: i32,
    chats: Vec<i64>,
    message: String,
    images: Vec<String>,
    datetime: String,
}

impl AppState {
    pub async fn fill_status_list(&self) -> anyhow::Result<()> {
        let actual_chats = self.get_chats().await?;
        for chat in &actual_chats {
            self.chats_status
                .entry(chat.id)
                .or_insert(ChatCleaningStatus::Idle);
        }

        Ok(())
    }

    pub fn set_chat_status(&self, chat_id: i64, status: ChatCleaningStatus) -> anyhow::Result<()> {
        *self
            .chats_status
            .get_mut(&chat_id)
            .context("Chat not found")? = status;

        Ok(())
    }

    pub async fn get_chats(&self) -> anyhow::Result<Chats> {
        let chats = sqlx::query_as!(
            Chat,
            r#"
SELECT id, name FROM tg_chat 
            "#
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(chats)
    }

    // pub async fn get_status(&self) -> anyhow::Result<DashMap<i64, ChatCleaningStatus>> {
    //     Ok(self.chats_status)
    // }

    pub async fn new_chat(&self, chat: &teloxide::types::Chat) -> anyhow::Result<()> {
        info!("adding a new chat:{chat:?}");
        let id = chat.id.0;
        let name = chat.title().unwrap_or_default();

        self.chats_status
            .entry(id)
            .or_insert(ChatCleaningStatus::Idle);

        sqlx::query!(
            r#"
INSERT INTO tg_chat ( id, name )
VALUES ( $1, $2 )
ON CONFLICT (id) DO UPDATE
SET name = $2
            "#,
            id,
            name
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn delete_chat(&self, chat_id: i64) -> anyhow::Result<()> {
        info!("deleting a chat:{chat_id}");

        self.chats_status.remove(&chat_id);

        sqlx::query!(
            r#"
DELETE FROM tg_chat
WHERE id = $1
            "#,
            chat_id,
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn get_all_members(&self, chat_id: i64) -> anyhow::Result<Users> {
        let users = sqlx::query_as!(
            User,
            r#"
SELECT id, chat_id, username, name FROM tg_user
WHERE chat_id = $1
            "#,
            chat_id
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(users)
    }

    pub async fn new_chat_member(
        &self,
        chat_id: i64,
        member: &teloxide::types::User,
    ) -> anyhow::Result<()> {
        info!("adding a new chat member:{member:?} to chat:{chat_id} ...");
        let id = member.id.0 as i64;

        if id == self.bot.get_me().await?.id.0 as i64 {
            info!("ignoring self...");
            return Ok(());
        }

        let username = member.username.clone().unwrap_or_default();
        let name = member.full_name();

        let chat_member = self.bot.get_chat_member(ChatId(chat_id), member.id).await?;
        if chat_member.is_privileged() {
            info!("ignored an admin.");
            return Ok(());
        }

        sqlx::query!(
            r#"
INSERT INTO tg_user ( id, chat_id, username, name )
VALUES ( $1, $2, $3, $4 )
ON CONFLICT ( id, chat_id ) DO UPDATE
SET username = $3, name = $4
            "#,
            id,
            chat_id,
            username,
            name
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn new_chat_members(
        &self,
        chat_id: i64,
        members: Vec<teloxide::types::User>,
    ) -> anyhow::Result<()> {
        for member in members {
            self.new_chat_member(chat_id, &member).await?;
        }
        Ok(())
    }

    pub async fn remove_chat_member(
        &self,
        chat_id: i64,
        member: &teloxide::types::User,
    ) -> anyhow::Result<()> {
        info!("removing a chat member:{member:?} from chat:{chat_id}");
        let id = member.id.0 as i64;

        sqlx::query!(
            r#"
DELETE FROM tg_user
WHERE id = $1 AND chat_id = $2
            "#,
            id,
            chat_id,
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn remove_chat_member_by_id(&self, chat_id: i64, user_id: i64) -> anyhow::Result<()> {
        info!("removing a chat member by id:{user_id} from chat:{chat_id}");
        sqlx::query!(
            r#"
DELETE FROM tg_user
WHERE id = $1 AND chat_id = $2
            "#,
            user_id,
            chat_id,
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn cleanup_chat(&self, chat_id: i64) -> anyhow::Result<()> {
        info!("got a request to cleanup chat:{chat_id}");

        for user in self.get_all_members(chat_id).await? {
            let member = match {
                self.bot
                    .get_chat_member(ChatId(chat_id), UserId(user.id as u64))
                    .await
            } {
                Ok(member) => member,
                Err(err) => {
                    error!("error while getting a user: {err}");
                    continue;
                }
            };
            match member.kind {
                teloxide::types::ChatMemberKind::Restricted(_)
                | teloxide::types::ChatMemberKind::Member => (),
                kind => {
                    info!("removing member with kind:{kind:?}");
                    self.remove_chat_member_by_id(chat_id, user.id).await?;
                }
            }
        }

        info!("done cleaning up!");

        Ok(())
    }

    pub async fn delete_all_members(&self, chat_id: i64) -> anyhow::Result<()> {
        info!("deleting all members from chat:{chat_id}");

        self.set_chat_status(chat_id, ChatCleaningStatus::InProgress)?;

        let chat = self.bot.get_chat(ChatId(chat_id)).await?;

        self.cleanup_chat(chat_id).await?;

        for user in self.get_all_members(chat_id).await? {
            let ban_result = if chat.is_supergroup() || chat.is_channel() {
                self.bot
                    .unban_chat_member(ChatId(chat_id), UserId(user.id as u64))
                    .await
            } else {
                self.bot
                    .kick_chat_member(ChatId(chat_id), UserId(user.id as u64))
                    .await
            };
            match ban_result {
                Ok(_) => {
                    self.remove_chat_member_by_id(chat_id, user.id).await?;
                }
                Err(err) => {
                    error!("Failed to delete a user:{user:?}! {err}");
                }
            };
        }

        self.set_chat_status(chat_id, ChatCleaningStatus::Idle)?;

        info!("done deleting all member from chat:{chat_id}");

        Ok(())
    }

    pub async fn clear_chats(&self, chats: Vec<i64>) -> anyhow::Result<()> {
        let mut chats_to_clean = Vec::new();
        for chat_id in chats {
            let mut status = self
                .chats_status
                .get_mut(&chat_id)
                .context("chat not found")?;
            let valmut = status.value_mut();
            match valmut {
                ChatCleaningStatus::Idle | ChatCleaningStatus::Error(_) => {
                    // self.set_chat_status(chat_id, ChatCleaningStatus::Queued)?;
                    *valmut = ChatCleaningStatus::Queued;
                    chats_to_clean.push(chat_id);
                }
                ChatCleaningStatus::Queued | ChatCleaningStatus::InProgress => {}
            }
        }

        for chat_id in chats_to_clean {
            if let Err(e) = self.delete_all_members(chat_id).await {
                self.set_chat_status(chat_id, ChatCleaningStatus::Error(e.to_string()))?
            };
        }

        Ok(())
    }

    pub async fn send_media_group(
        &self,
        chat_id: i64,
        images: Vec<InputMedia>,
    ) -> anyhow::Result<()> {
        info!("sending images to chat:{chat_id}");

        match self.bot.send_media_group(ChatId(chat_id), images).await {
            Ok(_) => {
                info!("sent media group to chat {chat_id}");
            }
            Err(err) => {
                error!("error sending media group {err}");
            }
        }

        Ok(())
    }

    pub async fn send_message_to_chat(&self, chat_id: i64, message: &str) -> anyhow::Result<()> {
        info!("sending message:{message} to chat:{chat_id}");

        match self
            .bot
            .send_message(ChatId(chat_id), message)
            .parse_mode(ParseMode::MarkdownV2)
            .await
        {
            Ok(_) => {
                info!("sent message to chat {chat_id}")
            }
            Err(err) => {
                error!("error sending message {err}");
            }
        };

        Ok(())
    }

    pub async fn send_message_with_images_to_chats(
        &self,
        chats: Vec<i64>,
        message: String,
        images: Vec<String>,
    ) -> anyhow::Result<()> {
        let images: Vec<InputMedia> = images
            .into_iter()
            .map(|body| {
                // let (body, _) = DataUrl::process(&i).unwrap().decode_to_vec().unwrap();
                base64::engine::general_purpose::STANDARD.decode(body)
            })
            .collect::<Result<Vec<Vec<u8>>, _>>()?
            .into_iter()
            .map(|i| InputMedia::Photo(InputMediaPhoto::new(InputFile::memory(i))))
            .collect();

        for chat_id in chats {
            if images.len() > 0 {
                for chunk in images.chunks(10) {
                    self.send_media_group(chat_id, chunk.to_vec()).await?;
                }
            }
            self.send_message_to_chat(chat_id, &message).await?;
        }

        Ok(())
    }

    pub async fn queue_message_with_images(
        &self,
        chats: Vec<i64>,
        message: String,
        images: Vec<String>,
        datetime: String,
    ) -> anyhow::Result<()> {
        info!("queueing message: {message} on datetime: {datetime}");

        sqlx::query!(
            r#"
            INSERT INTO message_queue ( chats, message, images, datetime )
            VALUES ( $1, $2, $3, $4 )
            "#,
            &chats,
            message,
            &images,
            datetime
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn remove_queued_message(&self, id: i32) -> anyhow::Result<()> {
        info!("removing queued message: {id}");

        sqlx::query!(
            r#"
            DELETE FROM message_queue
            WHERE id = $1
            "#,
            id,
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn cleanup_deprecated_chats(state: Self) -> anyhow::Result<()> {
        loop {
            match state.cleanup_deprecated_chats_loop().await {
                Ok(_) => {
                    info!("cleaned up deprecated chats!");
                }
                Err(err) => {
                    error!("failed to clean deprecated chats: {err}");
                }
            }
            tokio::time::sleep(Duration::from_secs(300)).await;
        }
    }

    pub async fn cleanup_deprecated_chats_loop(&self) -> anyhow::Result<()> {
        let chats = self.get_chats().await?;

        for chat in chats {
            match self
                .bot
                .get_chat_member(ChatId(chat.id), self.bot.get_me().await?.id)
                .await
            {
                Ok(member) => {
                    // match member.is_present() {
                    // true => info!("bot is present in this chat"),
                    // false => todo!("bot is not present"),
                    // }
                    // info!("{member:?}")
                }
                Err(err) => match err {
                    teloxide::RequestError::Api(api_err) => match api_err {
                        teloxide::ApiError::ChatNotFound
                        | teloxide::ApiError::BotKicked
                        | teloxide::ApiError::BotKickedFromSupergroup => {
                            info!("found a deprecated chat:{} {api_err}", chat.id);
                            match self.delete_chat(chat.id).await {
                                Ok(_) => {
                                    info!("deleted deprecated chat: {}", chat.id)
                                }
                                Err(err) => {
                                    error!("failed to delete deprecated chat:{} {err}", chat.id)
                                }
                            };
                        }
                        _ => {
                            if api_err.to_string() == "Unknown error: \"Forbidden: bot was kicked from the group chat\"" {
                                    match self.delete_chat(chat.id).await {
                                        Ok(_) => {
                                            info!("deleted deprecated chat: {}", chat.id)
                                        }
                                        Err(err) => {
                                            error!("failed to delete deprecated chat:{} {err}", chat.id)
                                        }
                                    };
                                }
                            error!(
                                "bad api error when checking for deprecated chat:{}... {}",
                                chat.id,
                                api_err.to_string()
                            )
                        }
                    },
                    _ => {
                        error!(
                            "bad error when checking for deprecated chat:{}... {err}",
                            chat.id
                        )
                    }
                },
            }
        }

        Ok(())
    }

    pub async fn message_queue(state: Self) -> anyhow::Result<()> {
        loop {
            match state.message_queue_loop().await {
                Ok(_) => {
                    info!("looped through queued messages successfully")
                }
                Err(err) => {
                    error!("failed to process a message queue: {err}")
                }
            }
            tokio::time::sleep(Duration::from_secs(15)).await;
        }
    }

    async fn message_queue_loop(&self) -> anyhow::Result<()> {
        let mut messages = sqlx::query_as!(
            QueuedMessage,
            r#"
                SELECT * from message_queue
                "#
        )
        .fetch(&self.pool);

        while let Some(message) = messages.try_next().await? {
            match self.process_queued_message(message.clone()).await {
                Ok(_) => {}
                Err(err) => {
                    error!("failed to process through message {}: {err}", message.id);
                }
            }
        }

        Ok(())
    }

    async fn process_queued_message(&self, message: QueuedMessage) -> anyhow::Result<()> {
        let parsed_datetime = chrono::DateTime::parse_from_rfc3339(&message.datetime)?;
        if parsed_datetime < chrono::Utc::now() {
            info!("queued message {} is expired! sending it now!", message.id);
            self.send_message_with_images_to_chats(message.chats, message.message, message.images)
                .await?;
            self.remove_queued_message(message.id).await?;
        }

        Ok(())
    }

    pub async fn migrate_chat(&self, old_id: i64, new_id: i64) -> anyhow::Result<()> {
        info!("migrating chat {old_id} -> {new_id}");

        sqlx::query!(
            r#"
        UPDATE tg_chat
        SET id = $1
        WHERE id = $2
        "#,
            new_id,
            old_id
        )
        .execute(&self.pool)
        .await?;

        self.chats_status
            .remove(&old_id)
            .context("Tried to migrate from an unexisting status")?;

        self.chats_status
            .entry(new_id)
            .or_insert(ChatCleaningStatus::Idle);

        Ok(())
    }
}

pub type Chats = Vec<Chat>;

#[derive(Serialize)]
pub struct Chat {
    id: i64,
    name: String,
}

pub type Users = Vec<User>;

#[derive(Debug)]
pub struct User {
    id: i64,
    chat_id: i64,
    username: Option<String>,
    name: String,
}
