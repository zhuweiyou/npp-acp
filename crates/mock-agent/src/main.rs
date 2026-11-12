//! Minimal ACP mock agent over stdio, used to exercise the client end-to-end.
//! On each prompt it streams a handful of `agent_message_chunk` updates and
//! finishes with `end_turn`.

use agent_client_protocol::v1::schema::{
    AgentCapabilities, ContentBlock, ContentChunk, InitializeRequest, InitializeResponse,
    NewSessionRequest, NewSessionResponse, PromptRequest, PromptResponse, SessionNotification,
    SessionUpdate, StopReason, TextContent,
};
use agent_client_protocol::{Agent, Result, Stdio};

#[tokio::main]
async fn main() -> Result<()> {
    Agent
        .v1()
        .name("mock-agent")
        .on_receive_request(
            async move |init: InitializeRequest, responder, _cx| {
                responder.respond(
                    InitializeResponse::new(init.protocol_version)
                        .agent_capabilities(AgentCapabilities::new()),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |_req: NewSessionRequest, responder, _cx| {
                responder.respond(NewSessionResponse::new("mock-session-1"))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |prompt: PromptRequest, responder, connection| {
                let session_id = prompt.session_id.clone();
                let user_text = prompt
                    .prompt
                    .iter()
                    .find_map(|block| match block {
                        ContentBlock::Text(text) => Some(text.text.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();

                let chunks = [
                    "你好，我是 mock ACP agent。".to_string(),
                    format!("\n\n收到你的消息：\"{user_text}\""),
                    "\n\n下面演示流式输出：".to_string(),
                    "一、二、三……完成 ✅".to_string(),
                ];

                for (i, chunk) in chunks.into_iter().enumerate() {
                    if i > 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(280)).await;
                    }
                    let notification = SessionNotification::new(
                        session_id.clone(),
                        SessionUpdate::AgentMessageChunk(ContentChunk::new(
                            ContentBlock::Text(TextContent::new(chunk)),
                        )),
                    );
                    if let Err(err) = connection.send_notification(notification) {
                        eprintln!("[mock-agent] send_notification failed: {err}");
                        break;
                    }
                }

                responder.respond(PromptResponse::new(StopReason::EndTurn))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_to(Stdio::new())
        .await
}
