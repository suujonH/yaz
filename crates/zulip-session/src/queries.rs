//! Favorites, chat history, files and search share one message query: the same narrow construction,
//! paging, content filtering and grouping.

use std::collections::HashSet;
use std::sync::Arc;

use futures::StreamExt;
use zulip_api::{ApiError, MessageQuery, NarrowTerm};
use zulip_core::ConversationKey;
use zulip_core::file::FileKind;
use zulip_core::html::{self, InlineImage, UploadLink};
use zulip_core::model::{AttachmentList, Channel, ChatMessage, PinItem, PinScope};
use zulip_core::text::time::date_label;
use zulip_store::DownloadRecord;

use crate::error::Result;
use crate::session::{Session, narrow_for};
use crate::state::State;

/// Lowest feature level that supports `anchor=date`.
const ANCHOR_DATE_LEVEL: i32 = 445;
pub const DEFAULT_PAGE_SIZE: u32 = 50;
pub const FILES_PAGE_SIZE: u32 = 100;
const SEARCH_MESSAGES: u32 = 200;
const SENDER_RANKING_SIZE: u32 = 200;
const REFERENCE_FETCH_LIMIT: usize = 8;
const SEARCH_LIST_LIMIT: usize = 50;
const SEARCH_TOPICS_PER_RUN: usize = 50;

/// Where to search.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum QueryScope {
    #[default]
    Everywhere,
    Conversation(ConversationKey),
    Starred,
    Sender(i64),
}

/// Filter by content type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ContentFilter {
    #[default]
    All,
    /// Plain chat without images, attachments or links.
    PlainChat,
    /// Images and videos.
    Media,
    /// Files other than images and videos.
    Files,
    /// Any attachment.
    Attachments,
    Links,
}

/// Query conditions.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct MessageSearch {
    pub scope: QueryScope,
    pub filter: ContentFilter,
    pub text: String,
    pub sender: Option<i64>,
    /// Only this time (ISO 8601) and earlier. Ignored when the server does not support it.
    pub until: Option<String>,
}

/// One message of the query result.
#[derive(Clone, Debug, PartialEq)]
pub struct MessageItem {
    pub message: Arc<ChatMessage>,
    /// HTML with highlighting when searching, otherwise the message HTML.
    pub html: String,
    pub images: Vec<InlineImage>,
    pub uploads: Vec<UploadLink>,
}

impl MessageItem {
    /// Summary in the list: a thumbnail when there is an image, a file when there is only an
    /// attachment, otherwise text.
    pub fn preview(&self) -> ItemPreview {
        if let Some(image) = self.images.first() {
            return ItemPreview::Thumbnail(image.src.clone());
        }
        if let Some(upload) = self.uploads.iter().find(|u| u.is_image) {
            return ItemPreview::Thumbnail(upload.path.clone());
        }
        if html::is_attachment_only(&self.message.content_html)
            && let Some(upload) = self.uploads.first()
        {
            return ItemPreview::File(upload.clone());
        }
        ItemPreview::Text(html::to_plain_text(&self.message.content_html))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ItemPreview {
    Text(String),
    Thumbnail(String),
    File(UploadLink),
}

fn is_media_upload(upload: &UploadLink) -> bool {
    upload.is_image || html::is_video_path(&upload.path)
}

fn passes_client_filter(item: &MessageItem, filter: ContentFilter) -> bool {
    match filter {
        ContentFilter::Media => !item.images.is_empty() || item.uploads.iter().any(is_media_upload),
        ContentFilter::Files => item.uploads.iter().any(|u| !is_media_upload(u)),
        _ => true,
    }
}

/// Query conditions -> narrow. Content filters that can be done on the server are done here; the
/// rest in [`passes_client_filter`].
pub fn build_narrow(state: &State, search: &MessageSearch) -> Vec<NarrowTerm> {
    let mut narrow = match &search.scope {
        QueryScope::Everywhere => Vec::new(),
        QueryScope::Conversation(key) => narrow_for(state, key),
        QueryScope::Starred => vec![NarrowTerm::is("starred")],
        QueryScope::Sender(id) => vec![NarrowTerm::sender(*id)],
    };
    match search.filter {
        ContentFilter::All => {}
        ContentFilter::PlainChat => {
            narrow.push(NarrowTerm::has("image", true));
            narrow.push(NarrowTerm::has("attachment", true));
            narrow.push(NarrowTerm::has("link", true));
        }
        ContentFilter::Media | ContentFilter::Files | ContentFilter::Attachments => {
            narrow.push(NarrowTerm::has("attachment", false));
        }
        ContentFilter::Links => {
            narrow.push(NarrowTerm::has("link", false));
            narrow.push(NarrowTerm::has("attachment", true));
        }
    }
    if let Some(sender) = search.sender {
        narrow.push(NarrowTerm::sender(sender));
    }
    let text = search.text.trim();
    if !text.is_empty() {
        narrow.push(NarrowTerm::search(text));
    }
    narrow
}

/// Which messages [`Session::message_context`] fetches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextDirection {
    Around,
    Older,
    Newer,
}

/// A page of [`Session::message_context`]: messages oldest first, and whether either end of the
/// conversation was reached.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ContextPage {
    pub items: Vec<MessageItem>,
    pub found_oldest: bool,
    pub found_newest: bool,
}

/// Paging cursor from newest to oldest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessagePager {
    pub search: MessageSearch,
    pub page_size: u32,
    anchor: Option<i64>,
    done: bool,
}

impl MessagePager {
    pub fn new(search: MessageSearch) -> Self {
        Self::with_page_size(search, DEFAULT_PAGE_SIZE)
    }

    pub fn with_page_size(search: MessageSearch, page_size: u32) -> Self {
        Self {
            search,
            page_size,
            anchor: None,
            done: false,
        }
    }

    /// Continues from the position returned by [`cursor`](Self::cursor).
    pub fn resume(search: MessageSearch, page_size: u32, cursor: i64) -> Self {
        Self {
            anchor: Some(cursor),
            ..Self::with_page_size(search, page_size)
        }
    }

    pub fn is_done(&self) -> bool {
        self.done
    }

    /// Start of the next page; `None` if nothing has been fetched yet.
    pub fn cursor(&self) -> Option<i64> {
        self.anchor
    }
}

impl Session {
    /// Next page (newest first). If a whole page is filtered out by the content filter, keeps
    /// fetching until there is a result or the end is reached.
    pub async fn next_page(&self, pager: &mut MessagePager) -> Result<Vec<MessageItem>> {
        let mut items = Vec::new();
        while items.is_empty() && !pager.done {
            let (narrow, feature_level) = {
                let state = self.state.read();
                (
                    build_narrow(&state, &pager.search),
                    state.realm.feature_level,
                )
            };
            let mut query = match pager.anchor {
                Some(anchor) => {
                    let mut q = MessageQuery::new(narrow, anchor, pager.page_size, 0);
                    q.include_anchor = false;
                    q
                }
                None => match pager
                    .search
                    .until
                    .as_ref()
                    .filter(|_| feature_level >= ANCHOR_DATE_LEVEL)
                {
                    Some(until) => {
                        let mut q = MessageQuery::new(narrow, "date", pager.page_size, 0);
                        q.anchor_date = Some(until.clone());
                        q
                    }
                    None => MessageQuery::new(narrow, "newest", pager.page_size, 0),
                },
            };
            query.apply_markdown = true;
            let result = self.client.get_messages(&query).await?;
            pager.anchor = result.messages.first().map(|m| m.id);
            pager.done = result.found_oldest || result.messages.is_empty();
            let prepared = self.prepare_all(&result.messages);
            let state = self.state.read();
            items = result
                .messages
                .iter()
                .zip(prepared)
                .rev()
                .filter(|(_, m)| !state.messages.is_hidden(m))
                .map(|(dto, m)| {
                    let shown = dto
                        .match_content
                        .clone()
                        .unwrap_or_else(|| m.content_html.clone());
                    MessageItem {
                        images: html::extract_inline_images(&m.content_html),
                        uploads: html::extract_uploads(&m.content_html),
                        html: shown,
                        message: Arc::new(m),
                    }
                })
                .filter(|item| passes_client_filter(item, pager.search.filter))
                .collect();
        }
        Ok(items)
    }

    /// Messages of a conversation next to `anchor`, oldest first, for viewing a search hit in
    /// context. [`ContextDirection::Around`] fetches `count` on each side with the anchor itself;
    /// the others fetch `count` beyond the anchor, which is left out.
    pub async fn message_context(
        &self,
        key: &ConversationKey,
        anchor: i64,
        direction: ContextDirection,
        count: u32,
    ) -> Result<ContextPage> {
        let narrow = {
            let state = self.state.read();
            narrow_for(&state, key)
        };
        let (before, after) = match direction {
            ContextDirection::Around => (count, count),
            ContextDirection::Older => (count, 0),
            ContextDirection::Newer => (0, count),
        };
        let mut query = MessageQuery::new(narrow, anchor, before, after);
        query.include_anchor = direction == ContextDirection::Around;
        let result = self.client.get_messages(&query).await?;
        let prepared = self.prepare_all(&result.messages);
        let state = self.state.read();
        let items = result
            .messages
            .iter()
            .zip(prepared)
            .filter(|(_, m)| !state.messages.is_hidden(m))
            .map(|(_, m)| MessageItem {
                images: html::extract_inline_images(&m.content_html),
                uploads: html::extract_uploads(&m.content_html),
                html: m.content_html.clone(),
                message: Arc::new(m),
            })
            .collect();
        Ok(ContextPage {
            items,
            found_oldest: result.found_oldest,
            found_newest: result.found_newest,
        })
    }

    /// Searches messages, grouped by conversation.
    pub async fn search_message_groups(&self, text: &str) -> Result<Vec<MessageGroup>> {
        let search = MessageSearch {
            text: text.to_owned(),
            ..MessageSearch::default()
        };
        let mut pager = MessagePager::with_page_size(search, SEARCH_MESSAGES);
        let items = self.next_page(&mut pager).await?;
        let state = self.state.read();
        Ok(group_by_conversation(&state, items, !pager.is_done()))
    }

    pub fn search_contacts(&self, text: &str) -> Vec<ContactHit> {
        search_contacts(&self.state.read(), text)
    }

    /// Searches subscribed channels and loaded topics.
    pub fn search_channels(&self, text: &str) -> Vec<ChannelHit> {
        search_channels(&self.state.read(), text)
    }

    /// Query result -> file list.
    pub fn file_entries(&self, items: &[MessageItem]) -> Vec<FileEntry> {
        file_entries(&self.state.read(), items)
    }

    /// Download records -> file list. The time is when the file was last opened (or downloaded);
    /// the sender is known only while the message is cached.
    pub fn download_entries(&self, records: Vec<DownloadRecord>) -> Vec<FileEntry> {
        let state = self.state.read();
        records
            .into_iter()
            .filter_map(|record| {
                let key: ConversationKey = record.conv_key.parse().ok()?;
                let sender = state.messages.get(record.message_id).map(|m| m.sender_id);
                Some(FileEntry {
                    kind: FileKind::of(&record.upload_path),
                    upload: html::upload_link_of(&record.upload_path, Some(&record.file_name)),
                    message_id: record.message_id,
                    source: file_source(&state, sender, &key),
                    key,
                    timestamp: record.last_opened_at.unwrap_or(record.downloaded_at),
                    size: u64::try_from(record.size).ok(),
                })
            })
            .collect()
    }

    /// Candidates when filtering files by sender: yourself first, the rest by name.
    pub fn file_senders(&self) -> Vec<i64> {
        file_senders(&self.state.read())
    }

    /// Reorders candidate senders by who sent files most recently.
    pub async fn rank_file_senders(&self) -> Result<Vec<i64>> {
        let query = MessageQuery::new(
            vec![NarrowTerm::has("attachment", false)],
            "newest",
            SENDER_RANKING_SIZE,
            0,
        );
        let result = self.client.get_messages(&query).await?;
        let recent: Vec<i64> = result.messages.iter().rev().map(|m| m.sender_id).collect();
        let mut senders = self.file_senders();
        senders.sort_by_key(|id| recent.iter().position(|r| r == id).unwrap_or(usize::MAX));
        Ok(senders)
    }

    /// Candidates when filtering files by conversation, in the same order as the message list.
    pub fn file_conversations(&self, topics_only: bool) -> Vec<ConversationKey> {
        file_conversations(&self.state.read(), topics_only)
    }

    /// All files I uploaded.
    pub async fn attachments(&self) -> Result<AttachmentList> {
        Ok(self.client.get_attachments().await?)
    }

    /// Messages that reference an uploaded file, ordered by send time. Fetched from the server one
    /// by one (at most [`REFERENCE_FETCH_LIMIT`] at a time). Messages the server says cannot be
    /// fetched (deleted, no permission) are skipped; other failures are counted in `failed`.
    pub async fn message_references(&self, message_ids: &[i64]) -> MessageReferences {
        let fetched = fetch_limited(message_ids, |id| self.client.get_message(id, true)).await;
        let state = self.state.read();
        let mut failed = 0;
        let mut references: Vec<MessageReference> = fetched
            .into_iter()
            .zip(message_ids)
            .filter_map(|(result, id)| {
                let dto = result
                    .inspect_err(|error| {
                        log::warn!("取得消息失败：{id}：{error}");
                        if !is_message_unavailable(error) {
                            failed += 1;
                        }
                    })
                    .ok()?;
                let message = state.prepare_message(&dto);
                Some(MessageReference {
                    title: state.display_title(&message.key, false),
                    preview: html::to_preview(&message.content_html),
                    message_id: message.id,
                    timestamp: message.timestamp,
                    key: message.key,
                })
            })
            .collect();
        references.sort_by_key(|r| (r.timestamp, r.message_id));
        MessageReferences { references, failed }
    }
}

/// Fetches results in the order of `ids`, with at most [`REFERENCE_FETCH_LIMIT`] in flight.
async fn fetch_limited<T, F: Future<Output = T>>(
    ids: &[i64],
    fetch: impl FnMut(i64) -> F,
) -> Vec<T> {
    futures::stream::iter(ids.iter().copied())
        .map(fetch)
        .buffered(REFERENCE_FETCH_LIMIT)
        .collect()
        .await
}

/// The server explicitly replied that the message does not exist or is not visible (not a network,
/// rate-limit or server failure).
fn is_message_unavailable(error: &ApiError) -> bool {
    matches!(
        error,
        ApiError::Api {
            status: 400 | 403 | 404,
            ..
        }
    )
}

/// Result of [`Session::message_references`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MessageReferences {
    pub references: Vec<MessageReference>,
    /// Number of messages that could not be fetched due to the network or similar causes.
    pub failed: usize,
}

/// One message that references an uploaded file.
#[derive(Clone, Debug, PartialEq)]
pub struct MessageReference {
    pub key: ConversationKey,
    pub message_id: i64,
    /// Conversation title (same as in the conversation list).
    pub title: String,
    /// Unix seconds.
    pub timestamp: i64,
    /// Single-line content preview.
    pub preview: String,
}

/// Messages of the same day.
#[derive(Clone, Debug, PartialEq)]
pub struct DateGroup<'a> {
    /// Date for display, `yyyy/MM/dd` (local time).
    pub date: String,
    pub items: &'a [MessageItem],
}

/// Groups by date, keeping the input order. Adjacent messages of the same day form one group.
pub fn group_by_date(items: &[MessageItem]) -> Vec<DateGroup<'_>> {
    let mut groups = Vec::new();
    let mut start = 0;
    while start < items.len() {
        let date = date_label(items[start].message.timestamp);
        let len = items[start..]
            .iter()
            .take_while(|item| date_label(item.message.timestamp) == date)
            .count();
        groups.push(DateGroup {
            date,
            items: &items[start..start + len],
        });
        start += len;
    }
    groups
}

/// One conversation in the search results.
#[derive(Clone, Debug, PartialEq)]
pub struct MessageGroup {
    pub key: ConversationKey,
    pub title: String,
    /// "N related chat records" (the Chinese label with the count N), or "N+" when there are more.
    pub count_text: String,
    pub items: Vec<MessageItem>,
}

fn group_by_conversation(
    state: &State,
    items: Vec<MessageItem>,
    has_more: bool,
) -> Vec<MessageGroup> {
    let mut groups: Vec<MessageGroup> = Vec::new();
    for item in items {
        match groups.iter_mut().find(|g| g.key == item.message.key) {
            Some(group) => group.items.push(item),
            None => groups.push(MessageGroup {
                key: item.message.key.clone(),
                title: state.display_title(&item.message.key, false),
                count_text: String::new(),
                items: vec![item],
            }),
        }
    }
    let max_id = |g: &MessageGroup| g.items.iter().map(|i| i.message.id).max().unwrap_or(0);
    groups.sort_by_key(|g| std::cmp::Reverse(max_id(g)));
    for group in &mut groups {
        let plus = if has_more { "+" } else { "" };
        group.count_text = format!("{}{plus}条相关聊天记录", group.items.len());
    }
    groups
}

/// Contacts found by search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContactHit {
    pub user_id: i64,
    pub display_name: String,
    pub avatar_url: Option<String>,
}

fn search_contacts(state: &State, text: &str) -> Vec<ContactHit> {
    let query = text.trim().to_lowercase();
    if query.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<(bool, ContactHit)> = state
        .users
        .all()
        .filter(|u| u.is_active && Some(u.id) != state.users.notification_bot_id)
        .filter_map(|u| {
            let display = state.display_name(u.id);
            let fields = [
                display.to_lowercase(),
                u.full_name.to_lowercase(),
                u.email.to_lowercase(),
            ];
            if !fields.iter().any(|f| f.contains(&query)) {
                return None;
            }
            let prefix = fields.iter().any(|f| f.starts_with(&query));
            Some((
                prefix,
                ContactHit {
                    user_id: u.id,
                    display_name: display,
                    avatar_url: u.avatar_url.clone(),
                },
            ))
        })
        .collect();
    hits.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.display_name.cmp(&b.1.display_name))
    });
    hits.truncate(SEARCH_LIST_LIMIT);
    hits.into_iter().map(|(_, hit)| hit).collect()
}

/// Channel or topic found by search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChannelHit {
    Channel {
        stream_id: i64,
        /// `#name`
        title: String,
        /// "N topics" (the Chinese label with the count N); "channel" (the Chinese label) when topics are
        /// not loaded.
        subtitle: String,
    },
    Topic {
        key: ConversationKey,
        title: String,
        /// "Channel: #name" (the Chinese label, then a fullwidth colon U+FF1A).
        subtitle: String,
        max_id: i64,
    },
}

fn search_channels(state: &State, text: &str) -> Vec<ChannelHit> {
    let query = text.trim().to_lowercase();
    if query.is_empty() {
        return Vec::new();
    }
    let subscribed: Vec<Channel> = state.channels.subscribed();
    let mut hits = Vec::new();
    for channel in &subscribed {
        let name = state.channel_display_name(channel.stream_id);
        if name.to_lowercase().contains(&query) || channel.name.to_lowercase().contains(&query) {
            let subtitle = match state.channels.topics(channel.stream_id) {
                Some(topics) => format!("{}个话题", topics.len()),
                None => "频道".to_owned(),
            };
            hits.push(ChannelHit::Channel {
                stream_id: channel.stream_id,
                title: format!("#{name}"),
                subtitle,
            });
        }
    }
    let mut topics = Vec::new();
    for channel in &subscribed {
        let name = state.channel_display_name(channel.stream_id);
        for topic in state.channels.topics(channel.stream_id).unwrap_or_default() {
            if topic.name.to_lowercase().contains(&query) {
                topics.push(ChannelHit::Topic {
                    key: ConversationKey::topic(channel.stream_id, topic.name.clone()),
                    title: topic.name.clone(),
                    subtitle: format!("频道：#{name}"),
                    max_id: topic.max_id,
                });
            }
        }
    }
    topics.sort_by_key(|hit| match hit {
        ChannelHit::Topic { max_id, .. } => std::cmp::Reverse(*max_id),
        ChannelHit::Channel { .. } => std::cmp::Reverse(0),
    });
    topics.truncate(SEARCH_TOPICS_PER_RUN);
    hits.extend(topics);
    hits
}

/// One row of the file list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    pub upload: UploadLink,
    pub kind: FileKind,
    pub message_id: i64,
    pub key: ConversationKey,
    pub timestamp: i64,
    /// "Sent to <conversation>" or "From <conversation> | <sender>" (the Chinese labels).
    pub source: String,
    /// Bytes; `None` when unknown, filled in after querying with `Session::file_size`.
    pub size: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FileSort {
    #[default]
    Time,
    Name,
    Size,
}

fn file_entries(state: &State, items: &[MessageItem]) -> Vec<FileEntry> {
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    for item in items {
        let m = &item.message;
        for upload in &item.uploads {
            if !seen.insert(upload.path.clone()) {
                continue;
            }
            let source = file_source(state, Some(m.sender_id), &m.key);
            entries.push(FileEntry {
                kind: FileKind::of(&upload.path),
                upload: upload.clone(),
                message_id: m.id,
                key: m.key.clone(),
                timestamp: m.timestamp,
                source,
                size: None,
            });
        }
    }
    entries
}

/// Where a file came from: "Sent to <conversation>" when I sent it, otherwise "From <conversation> |
/// <sender>" (the Chinese labels). The sender is left out when unknown or when it is the
/// conversation's title (direct messages).
fn file_source(state: &State, sender: Option<i64>, key: &ConversationKey) -> String {
    let title = state.display_title(key, false);
    match sender {
        Some(id) if id == state.users.self_id => format!("发给 {title}"),
        Some(id) => {
            let name = state.display_name(id);
            if name == title {
                format!("来自 {title}")
            } else {
                format!("来自 {title} | {name}")
            }
        }
        None => format!("来自 {title}"),
    }
}

fn file_senders(state: &State) -> Vec<i64> {
    let self_id = state.users.self_id;
    let mut users: Vec<(bool, String, i64)> = state
        .users
        .all()
        .filter(|u| u.is_active && Some(u.id) != state.users.notification_bot_id)
        .map(|u| (u.id != self_id, state.display_name(u.id), u.id))
        .collect();
    users.sort();
    users.into_iter().map(|(_, _, id)| id).collect()
}

fn file_conversations(state: &State, topics_only: bool) -> Vec<ConversationKey> {
    let mut rows: Vec<(&ConversationKey, Option<i64>, i64)> = state
        .conversations
        .iter()
        .filter(|(key, e)| {
            e.in_list
                && if topics_only {
                    matches!(key, ConversationKey::Topic(_))
                } else {
                    true
                }
        })
        .map(|(key, e)| {
            let pinned = state
                .pins
                .get(PinScope::Messages, &PinItem::Conversation(key.clone()));
            (key, pinned, e.last_message_time)
        })
        .collect();
    rows.sort_by_key(|(_, pinned, time)| {
        (
            pinned.is_none(),
            std::cmp::Reverse(*pinned),
            std::cmp::Reverse(*time),
        )
    });
    rows.into_iter().map(|(key, _, _)| key.clone()).collect()
}

/// Sorts the file list. Time: newest first; name: ascending; size: largest first (unknown last).
pub fn sort_files(entries: &mut [FileEntry], sort: FileSort) {
    match sort {
        FileSort::Time => entries.sort_by_key(|e| std::cmp::Reverse(e.timestamp)),
        FileSort::Name => entries.sort_by(|a, b| a.upload.file_name.cmp(&b.upload.file_name)),
        FileSort::Size => entries.sort_by_key(|e| std::cmp::Reverse(e.size)),
    }
}

pub fn file_count_text(count: usize) -> String {
    format!("共 {count} 个文件")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_support::state;

    #[tokio::test]
    async fn fetch_limited_keeps_order_and_limits_concurrency() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (running, peak) = (AtomicUsize::new(0), AtomicUsize::new(0));
        let ids: Vec<i64> = (0..40).collect();
        let fetched = fetch_limited(&ids, |id| {
            let (running, peak) = (&running, &peak);
            async move {
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                // Higher sequence numbers finish first, but the results still follow the order of ids.
                tokio::time::sleep(std::time::Duration::from_millis(40 - id as u64)).await;
                running.fetch_sub(1, Ordering::SeqCst);
                id
            }
        })
        .await;
        assert_eq!(fetched, ids);
        assert_eq!(peak.load(Ordering::SeqCst), REFERENCE_FETCH_LIMIT);
    }

    #[test]
    fn only_server_rejections_mean_the_message_is_unavailable() {
        let api = |status: u16| ApiError::Api {
            status,
            code: "BAD_REQUEST".to_owned(),
            message: String::new(),
            body: None,
        };
        for status in [400, 403, 404] {
            assert!(is_message_unavailable(&api(status)));
        }
        for status in [401, 429, 500, 502] {
            assert!(!is_message_unavailable(&api(status)));
        }
        assert!(!is_message_unavailable(&ApiError::Network(
            "断开".to_owned()
        )));
        assert!(!is_message_unavailable(&ApiError::Timeout(
            "超时".to_owned()
        )));
    }

    fn terms(narrow: &[NarrowTerm]) -> Vec<String> {
        narrow
            .iter()
            .map(|t| serde_json::to_string(t).unwrap())
            .collect()
    }

    fn item(id: i64, key: ConversationKey, ts: i64, html_text: &str) -> MessageItem {
        let mut m = ChatMessage::new(id, key, 2, ts);
        m.content_html = html_text.to_owned();
        MessageItem {
            images: html::extract_inline_images(html_text),
            uploads: html::extract_uploads(html_text),
            html: html_text.to_owned(),
            message: Arc::new(m),
        }
    }

    const IMAGE_HTML: &str = "<p><a href=\"/user_uploads/1/a/x.png\">x.png</a></p><div class=\"message_inline_image\"><a href=\"/user_uploads/1/a/x.png\"><img src=\"/user_uploads/1/a/x.png\"></a></div>";
    const FILE_HTML: &str = "<p><a href=\"/user_uploads/1/a/y.pdf\">y.pdf</a></p>";

    #[test]
    fn narrow_combines_scope_filter_sender_and_text() {
        let s = state();
        let search = MessageSearch {
            scope: QueryScope::Starred,
            filter: ContentFilter::Links,
            text: " 词 ".into(),
            sender: Some(5),
            until: None,
        };
        let joined = terms(&build_narrow(&s, &search)).join(" ");
        assert!(joined.contains("starred"));
        assert!(joined.contains("link"));
        assert!(joined.contains("attachment"));
        assert!(joined.contains("sender"));
        assert!(joined.contains("词"));
        assert!(!joined.contains(" 词 "));
    }

    #[test]
    fn plain_chat_excludes_everything_and_all_adds_nothing() {
        let s = state();
        let plain = MessageSearch {
            filter: ContentFilter::PlainChat,
            ..MessageSearch::default()
        };
        assert_eq!(build_narrow(&s, &plain).len(), 3);
        assert!(build_narrow(&s, &MessageSearch::default()).is_empty());
    }

    #[test]
    fn client_filter_separates_media_and_files() {
        let key = ConversationKey::topic(10, "t");
        let image = item(1, key.clone(), 1, IMAGE_HTML);
        let file = item(2, key, 1, FILE_HTML);
        assert!(passes_client_filter(&image, ContentFilter::Media));
        assert!(!passes_client_filter(&image, ContentFilter::Files));
        assert!(!passes_client_filter(&file, ContentFilter::Media));
        assert!(passes_client_filter(&file, ContentFilter::Files));
        assert!(passes_client_filter(&file, ContentFilter::All));
    }

    #[test]
    fn previews() {
        let key = ConversationKey::topic(10, "t");
        assert!(matches!(
            item(1, key.clone(), 1, IMAGE_HTML).preview(),
            ItemPreview::Thumbnail(_)
        ));
        assert!(matches!(
            item(1, key.clone(), 1, FILE_HTML).preview(),
            ItemPreview::File(_)
        ));
        assert_eq!(
            item(1, key, 1, "<p>文字</p>").preview(),
            ItemPreview::Text("文字".into())
        );
    }

    #[test]
    fn date_groups_keep_order() {
        let key = ConversationKey::topic(10, "t");
        let day = 86400 * 2;
        let items = [
            item(3, key.clone(), day * 100, "a"),
            item(2, key.clone(), day * 100, "b"),
            item(1, key, day * 50, "c"),
        ];
        let groups = group_by_date(&items);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].items.len(), 2);
        assert_eq!(groups[0].date.len(), 10);
    }

    #[test]
    fn conversation_groups_sort_by_latest_and_count() {
        let s = state();
        let (a, b) = (
            ConversationKey::topic(10, "a"),
            ConversationKey::topic(10, "b"),
        );
        let groups = group_by_conversation(
            &s,
            vec![
                item(1, a.clone(), 1, "x"),
                item(5, b.clone(), 1, "y"),
                item(2, a.clone(), 1, "z"),
            ],
            true,
        );
        assert_eq!(groups[0].key, b);
        assert_eq!(groups[1].key, a);
        assert_eq!(groups[1].count_text, "2+条相关聊天记录");
        assert_eq!(
            group_by_conversation(&s, vec![item(1, b, 1, "x")], false)[0].count_text,
            "1条相关聊天记录"
        );
    }

    #[test]
    fn contact_search_prefix_first_and_excludes_bot() {
        let mut s = state();
        s.remarks
            .set(&zulip_core::model::RemarkTarget::User(3), Some("杰的朋友"));
        let hits = search_contacts(&s, "杰");
        assert_eq!(hits.iter().map(|h| h.user_id).collect::<Vec<_>>(), [3, 2]);
        assert!(search_contacts(&s, "bot").is_empty());
        assert!(search_contacts(&s, "  ").is_empty());
        assert_eq!(search_contacts(&s, "a@x")[0].user_id, 2);
    }

    #[test]
    fn channel_search() {
        let mut s = state();
        s.channels.set_topics(
            10,
            &[
                zulip_api::TopicDto {
                    name: "开发计划".into(),
                    max_id: 1,
                },
                zulip_api::TopicDto {
                    name: "开发进度".into(),
                    max_id: 9,
                },
            ],
        );
        let hits = search_channels(&s, "开发");
        assert_eq!(hits.len(), 3);
        assert!(
            matches!(&hits[0], ChannelHit::Channel { title, subtitle, .. } if title == "#开发" && subtitle == "2个话题")
        );
        assert!(
            matches!(&hits[1], ChannelHit::Topic { title, subtitle, .. } if title == "开发进度" && subtitle == "频道：#开发")
        );
    }

    #[test]
    fn file_senders_put_self_first_and_skip_the_bot() {
        let s = state();
        let senders = file_senders(&s);
        assert_eq!(senders[0], 1);
        assert_eq!(senders.len(), 4);
        assert!(!senders.contains(&9));
    }

    #[test]
    fn file_entries_dedupe_source_and_sort() {
        let s = state();
        let key = ConversationKey::topic(10, "t");
        let items = [
            item(2, key.clone(), 2, FILE_HTML),
            item(1, key, 1, FILE_HTML),
        ];
        let mut entries = file_entries(&s, &items);
        assert_eq!(entries.len(), 1);
        assert!(entries[0].source.starts_with("来自 ") && entries[0].source.ends_with(" | 阿杰"));
        assert_eq!(entries[0].kind, FileKind::of("y.pdf"));
        entries[0].size = Some(3);
        let mut more = entries.clone();
        more.push(FileEntry {
            size: None,
            timestamp: 9,
            ..entries[0].clone()
        });
        sort_files(&mut more, FileSort::Time);
        assert_eq!(more[0].timestamp, 9);
        sort_files(&mut more, FileSort::Size);
        assert_eq!(more[0].size, Some(3));
        assert_eq!(file_count_text(2), "共 2 个文件");
    }
}
