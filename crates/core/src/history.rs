//! Penyimpanan history dalam memori (working memory kernel).
//!
//! Ini *bukan* state durable. Persistensi sesi hidup di host (store SQLite).

use crate::types::Message;

#[derive(Debug, Default, Clone)]
pub struct ContextStore {
    messages: Vec<Message>,
}

impl ContextStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_messages(messages: Vec<Message>) -> Self {
        Self { messages }
    }

    pub fn push(&mut self, message: Message) {
        self.messages.push(message);
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    pub fn last(&self) -> Option<&Message> {
        self.messages.last()
    }

    pub fn clear(&mut self) {
        self.messages.clear();
    }

    /// Ganti seluruh history (dipakai kompaksi).
    pub fn set_messages(&mut self, messages: Vec<Message>) {
        self.messages = messages;
    }
}
