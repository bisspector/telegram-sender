use teloxide::{
    dispatching::UpdateFilterExt,
    dptree,
    prelude::Dispatcher,
    requests::Requester,
    types::{Message, Update},
};
use tracing::{error, info, warn};

use crate::state::{AppState, WrappedBot};

pub async fn run(state: AppState) -> anyhow::Result<()> {
    info!("starting telegram bot...");

    let bot = state.bot.clone();

    // loop {
    //     let mut tasks = Vec::new();
    //     for i in 0..100 {
    //         let bot_clone = bot.clone();
    //         tasks.push(tokio::task::spawn(async move {
    //             match bot_clone
    //                 .send_message(ChatId(-735978806), format!("{} barbeque bacon burger", i))
    //                 .await
    //             {
    //                 Ok(_) => debug!("success!"),
    //                 Err(err) => error!("error here!!!: {err}"),
    //             }
    //         }));
    //     }
    //     join_all(tasks).await;
    //     debug!("LOOP");
    // }

    let handler = dptree::entry()
        .branch(Update::filter_message().endpoint(handle_message))
        .branch(Update::filter_edited_message().endpoint(handle_message));

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![state])
        .build()
        .dispatch()
        .await;

    Ok(())
}

async fn handle_message(message: Message, bot: WrappedBot, state: AppState) -> anyhow::Result<()> {
    info!("got a new message! {message:?}");

    if let teloxide::types::MessageKind::Common(m) = &message.kind {
        if let teloxide::types::MediaKind::Migration(teloxide::types::ChatMigration::From {
            chat_id: old_id,
        }) = m.media_kind
        {
            if let Some(teloxide::types::Chat { id: new_id, .. }) = m.sender_chat {
                state.migrate_chat(old_id.0, new_id.0).await?;
                return Ok(());
            } else {
                error!("Received migration without sender chat! :/");
            }
        }
    }

    let chat = &message.chat;
    let chat_id = chat.id.0;

    if chat.is_private() {
        return Ok(());
    }

    state.new_chat(&chat).await?;

    if let Some(user) = message.from() {
        state.new_chat_member(chat_id, user).await?;
    }

    match message.kind {
        teloxide::types::MessageKind::Common(_) => {
            //handle a basic message
        }
        teloxide::types::MessageKind::NewChatMembers(m) => {
            // handle a new chat member!
            state.new_chat_members(chat_id, m.new_chat_members).await?;
            bot.delete_message(message.chat.id, message.id).await?;
        }
        teloxide::types::MessageKind::LeftChatMember(m) => {
            state
                .remove_chat_member(chat_id, &m.left_chat_member)
                .await?;
            bot.delete_message(message.chat.id, message.id).await?;
        }
        teloxide::types::MessageKind::GroupChatCreated(_) => state.new_chat(&message.chat).await?,
        // teloxide::types::MessageKind::SupergroupChatCreated(_) => todo!(),
        _ => warn!("unhandled message type!"),
    }

    Ok(())
}
