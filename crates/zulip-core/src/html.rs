//! Message HTML rendered by Zulip: to plain text (conversation list preview, notifications, copy, plugins), extraction of uploaded files and inline images, block analysis.

use std::collections::{HashMap, HashSet};

use ego_tree::NodeRef;
use percent_encoding::percent_decode_str;
use scraper::node::{Node, Text};
use scraper::{ElementRef, Html};

use crate::text::{mention, time};
use crate::util::{
    ends_with_ignore_case, eq_ignore_case, file_extension, is_blank, starts_with_ignore_case,
    static_regex,
};

const UPLOADS_PREFIX: &str = "/user_uploads/";
const INLINE_IMAGE: &str = "message_inline_image";
const INLINE_VIDEO: &str = "message_inline_video";

/// File extensions of images (without the dot).
pub const IMAGE_EXTENSIONS: [&str; 6] = ["png", "jpg", "jpeg", "gif", "webp", "bmp"];
const VIDEO_EXTENSIONS: [&str; 3] = [".mp4", ".webm", ".mov"];
const BLOCK_TAGS: [&str; 21] = [
    "p",
    "div",
    "blockquote",
    "pre",
    "ul",
    "ol",
    "li",
    "table",
    "thead",
    "tbody",
    "tr",
    "td",
    "th",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "hr",
    "br",
];

static_regex!(WHITESPACE = r"\s+");
static_regex!(SPACES = r"[ \t\r\x0C\x0B]+");

/// An uploaded-file link in a message. `path` is a relative path starting with `/user_uploads/`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadLink {
    pub path: String,
    pub file_name: String,
    pub is_image: bool,
}

/// An inline image in a message: the display URL and the original link (the original link may be empty).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineImage {
    pub src: String,
    pub href: String,
}

impl InlineImage {
    /// URL for viewing the original image: for images uploaded to this organization it is the upload path, otherwise the display URL (external links go through the server proxy).
    pub fn original(&self) -> String {
        normalize_upload_path(&self.href).unwrap_or_else(|| self.src.clone())
    }
}

/// Parses message HTML. The content is the children of [`Html::root_element`].
pub fn parse_fragment(html: &str) -> Html {
    Html::parse_fragment(html)
}

fn has_class(e: ElementRef, class: &str) -> bool {
    e.value().classes().any(|c| c == class)
}

fn is_tag(e: ElementRef, tag: &str) -> bool {
    e.value().name() == tag
}

fn text_of(e: ElementRef) -> String {
    e.text().collect()
}

/// Descendant elements excluding the node itself, in document order.
fn descendants(e: ElementRef) -> impl Iterator<Item = ElementRef> {
    e.descendants().skip(1).filter_map(ElementRef::wrap)
}

fn first_descendant<'a>(e: ElementRef<'a>, tag: &str) -> Option<ElementRef<'a>> {
    descendants(e).find(|d| is_tag(*d, tag))
}

/// The URL of the first link with an href among the descendants.
fn first_href(e: ElementRef<'_>) -> Option<&str> {
    descendants(e)
        .filter(|d| is_tag(*d, "a"))
        .find_map(|a| a.value().attr("href"))
}

fn has_ancestor(e: ElementRef, predicate: impl Fn(ElementRef) -> bool) -> bool {
    e.ancestors().filter_map(ElementRef::wrap).any(predicate)
}

/// Non-video image preview elements (any element with `message_inline_image`) and their upload paths.
fn image_previews(root: ElementRef) -> impl Iterator<Item = (ElementRef, Option<String>)> {
    descendants(root)
        .filter(|e| has_class(*e, INLINE_IMAGE) && !has_class(*e, INLINE_VIDEO))
        .map(|e| (e, first_href(e).and_then(normalize_upload_path)))
}

/// Single-line preview: quote blocks are dropped; images, files, code, formulas and tables are replaced by the [image], [file]name, [code], [formula], [table] labels (Chinese UI text),
/// and runs of whitespace are merged into one space.
pub fn to_preview(html: &str) -> String {
    let doc = parse_fragment(html);
    let root = doc.root_element();
    let images = inline_preview_paths(root);
    let mut out = String::new();
    for child in root.children() {
        append_preview(child, &mut out, &images);
    }
    WHITESPACE.replace_all(&out, " ").trim().to_owned()
}

/// Multi-line plain text: paragraphs, line breaks and list items are separated by newlines; each line of a quote block is prefixed with "> "; code blocks keep their original text.
pub fn to_plain_text(html: &str) -> String {
    let mut doc = parse_fragment(html);
    let images = inline_preview_paths(doc.root_element());
    // An uploaded image in the same paragraph as text is shown at the link position: the [image] label stays in its own line
    let inlined: Vec<_> = inlined_previews(&doc)
        .into_iter()
        .map(|(link, preview)| (link.id(), preview.id()))
        .collect();
    for (link, preview) in inlined {
        if let Some(mut node) = doc.tree.get_mut(link) {
            node.insert_before(Node::Text(Text {
                text: "[图片]".into(),
            }));
            node.detach();
        }
        if let Some(mut node) = doc.tree.get_mut(preview) {
            node.detach();
        }
    }
    let mut lines = Vec::new();
    append_plain_blocks(doc.root_element(), &mut lines, &images);
    lines.join("\n").trim_matches('\n').to_owned()
}

/// The content is only image / video previews and uploaded-file links (no bubble background is shown in the chat).
pub fn is_attachment_only(html: &str) -> bool {
    let doc = parse_fragment(html);
    let mut any = false;
    for node in doc.root_element().children() {
        if let Node::Text(text) = node.value() {
            if !is_blank(text) {
                return false;
            }
            continue;
        }
        let Some(e) = ElementRef::wrap(node) else {
            continue;
        };
        let attachment = is_upload_preview(e)
            || is_thumbnail_gallery(e)
            || (is_tag(e, "p") && (is_upload_only_paragraph(e) || is_inline_image_paragraph(e)));
        if !attachment {
            return false;
        }
        any = true;
    }
    any
}

// The server puts consecutive thumbnails in one container: the container holds only previews of uploaded images
fn is_thumbnail_gallery(e: ElementRef) -> bool {
    if !has_class(e, "message-thumbnail-gallery") {
        return false;
    }
    let mut any = false;
    for node in e.children() {
        match node.value() {
            Node::Text(text) if is_blank(text) => {}
            _ => {
                if !ElementRef::wrap(node).is_some_and(is_upload_preview) {
                    return false;
                }
                any = true;
            }
        }
    }
    any
}

/// Inline previews of uploaded images / videos (links to `/user_uploads/` inside elements with `message_inline_image`).
pub fn is_upload_preview(e: ElementRef) -> bool {
    has_class(e, INLINE_IMAGE) && first_href(e).and_then(normalize_upload_path).is_some()
}

/// The paragraph contains only uploaded-file links (plus whitespace and line breaks).
pub fn is_upload_only_paragraph(p: ElementRef) -> bool {
    is_upload_only(p.children())
}

/// The inline content is only uploaded-file links (plus whitespace and line breaks): a paragraph, or text in a block such as a list item that is not inside a paragraph
/// (e.g. a list item like "- [a.png](...)" that contains only the link, with the image preview after the link).
pub fn is_upload_only<'a>(nodes: impl IntoIterator<Item = NodeRef<'a, Node>>) -> bool {
    only_inline(nodes, |e| {
        is_tag(e, "a")
            && e.value()
                .attr("href")
                .and_then(normalize_upload_path)
                .is_some()
    })
}

// The paragraph contains only body images (img.inline-image rendered from ![](...)) plus whitespace and line breaks
fn is_inline_image_paragraph(p: ElementRef) -> bool {
    only_inline(p.children(), |e| {
        is_tag(e, "img") && has_class(e, "inline-image")
    })
}

fn only_inline<'a>(
    nodes: impl IntoIterator<Item = NodeRef<'a, Node>>,
    accept: impl Fn(ElementRef) -> bool,
) -> bool {
    let mut any = false;
    for node in nodes {
        match node.value() {
            Node::Text(text) if is_blank(text) => {}
            _ => match ElementRef::wrap(node) {
                Some(e) if is_tag(e, "br") => {}
                Some(e) if accept(e) => any = true,
                _ => return false,
            },
        }
    }
    any
}

/// Display URL and original-image URL of a body image (`![](...)` rendered as `img.inline-image` in a paragraph):
/// while the thumbnail is not generated yet, src is a loading animation, so the original image is used instead.
pub fn inline_image_source(img: ElementRef<'_>) -> (Option<&str>, Option<&str>) {
    let original = img
        .value()
        .attr("data-original-src")
        .filter(|s| !s.is_empty());
    let src = match original {
        Some(o) if has_class(img, "image-loading-placeholder") => Some(o),
        _ => img.value().attr("src"),
    };
    (src, original.or(src))
}

/// Paths of uploaded links that have an inline preview; the file-name links in a paragraph that point to them are replaced by the preview.
/// Paths of uploaded-image links inside quote blocks. The server generates no previews for links inside quote blocks, so the client must show the images.
pub fn quoted_image_path(e: ElementRef, previews: &HashSet<String>) -> Option<String> {
    if !is_tag(e, "a") || !has_ancestor(e, |a| is_tag(a, "blockquote")) {
        return None;
    }
    normalize_upload_path(e.value().attr("href")?)
        .filter(|path| is_image_path(path) && !previews.contains(path))
}

pub fn inline_preview_paths(root: ElementRef) -> HashSet<String> {
    descendants(root)
        .filter(|e| has_class(*e, INLINE_IMAGE))
        .flat_map(descendants)
        .filter(|a| is_tag(*a, "a"))
        .filter_map(|a| a.value().attr("href").and_then(normalize_upload_path))
        .collect()
}

/// Uploaded images in the same paragraph as text (e.g. "screenshot [a.png](...)"): Zulip puts the preview after the paragraph, and the UI shows it at the link instead (on the same line as the text).
/// Returns such file-name links together with their image previews; for the same file only the first link is taken. Paragraphs with only upload links, and videos, are excluded (their previews are shown after the paragraph as usual).
pub fn inlined_previews(doc: &Html) -> Vec<(ElementRef<'_>, ElementRef<'_>)> {
    let root = doc.root_element();
    let mut previews = HashMap::new();
    for (preview, path) in image_previews(root) {
        if let Some(path) = path
            && first_descendant(preview, "img").is_some()
        {
            previews.entry(path).or_insert(preview);
        }
    }
    let mut result = Vec::new();
    let mut used = HashSet::new();
    for a in descendants(root).filter(|e| is_tag(*e, "a")) {
        let Some(path) = a.value().attr("href").and_then(normalize_upload_path) else {
            continue;
        };
        let Some(&preview) = previews.get(&path) else {
            continue;
        };
        let paragraph = a
            .ancestors()
            .filter_map(ElementRef::wrap)
            .find(|e| is_tag(*e, "p"));
        if is_file_name_label(&text_of(a), &path)
            && paragraph.is_some_and(|p| !is_upload_only_paragraph(p))
            && used.insert(path)
        {
            result.push((a, preview));
        }
    }
    result
}

/// Whether there are uploaded files that are not images.
pub fn has_file_attachment(html: &str) -> bool {
    extract_uploads(html).iter().any(|u| !u.is_image)
}

/// Extracts all uploaded-file links, de-duplicated by path. `is_image` follows the server: the server generated an image preview for it
/// (regardless of extension; uploaded files the server does not treat as images have no preview).
pub fn extract_uploads(html: &str) -> Vec<UploadLink> {
    let doc = parse_fragment(html);
    let root = doc.root_element();
    let images: HashSet<String> = image_previews(root).filter_map(|(_, path)| path).collect();
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for a in descendants(root).filter(|e| is_tag(*e, "a")) {
        let Some(path) = a.value().attr("href").and_then(normalize_upload_path) else {
            continue;
        };
        if seen.insert(path.clone()) {
            let is_image = images.contains(&path);
            result.push(UploadLink {
                is_image,
                ..upload_link_of(&path, Some(&text_of(a)))
            });
        }
    }
    result
}

/// Builds an [`UploadLink`] from a normalized upload path and the link text. The file name prefers the link text;
/// when the link text is empty, is a URL, or its extension differs from the upload path (e.g. a link text "report" for `.../report.pdf`),
/// the last segment of the path is used so that the downloaded file gets the right extension.
pub fn upload_link_of(path: &str, link_text: Option<&str>) -> UploadLink {
    let last = path.rsplit('/').next().unwrap_or(path);
    let path_name = percent_decode_str(last).decode_utf8_lossy().into_owned();
    let text = link_text.unwrap_or("").trim();
    let use_text = !text.is_empty()
        && !text.contains(UPLOADS_PREFIX)
        && !starts_with_ignore_case(text, "http")
        && eq_ignore_case(file_extension(text), file_extension(&path_name));
    UploadLink {
        path: path.to_owned(),
        file_name: if use_text { text.to_owned() } else { path_name },
        is_image: is_image_path(path),
    }
}

/// Whether the text of an upload link is just the file name (empty, the link itself, or ending with the same extension as the file).
/// With an inline preview only such text is hidden; descriptive text the user wrote (e.g. `[architecture diagram](/user_uploads/.../a.png)`) is kept.
pub fn is_file_name_label(text: &str, path: &str) -> bool {
    let text = text.trim();
    if text.is_empty() || text.contains(&UPLOADS_PREFIX[1..]) {
        return true;
    }
    let ext = file_extension(path);
    !ext.is_empty() && ends_with_ignore_case(text, ext)
}

/// Normalizes a link to a relative path of the form `/user_uploads/...` (query string removed); returns `None` if it is not an upload of this organization.
/// When rendering, the server rewrites absolute links to this organization as relative links (like `user_uploads/...`, without the leading slash;
/// verified on Zulip 12.3), so any link with a domain belongs to another server and is not treated as an uploaded file.
/// Thumbnail paths (`/user_uploads/thumbnail/...`) do not count as uploaded files.
pub fn normalize_upload_path(href: &str) -> Option<String> {
    let mut path = if href.starts_with(UPLOADS_PREFIX) {
        href.to_owned()
    } else if href.starts_with(&UPLOADS_PREFIX[1..]) {
        format!("/{href}")
    } else {
        return None;
    };
    if let Some(cut) = path.find(['?', '#']) {
        path.truncate(cut);
    }
    (!path.starts_with("/user_uploads/thumbnail/")).then_some(path)
}

/// The original path of an uploaded file; for a thumbnail URL (`/user_uploads/thumbnail/{path}/{size}.{format}`) it is the path of the original image.
pub fn original_upload_path(href: &str) -> Option<String> {
    if let Some(path) = normalize_upload_path(href) {
        return Some(path);
    }
    let rest = href.strip_prefix("/user_uploads/thumbnail/")?;
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    let (original, _size) = rest.rsplit_once('/')?;
    (!original.is_empty()).then(|| format!("{UPLOADS_PREFIX}{original}"))
}

pub fn is_image_path(path: &str) -> bool {
    let ext = file_extension(path).trim_start_matches('.');
    IMAGE_EXTENSIONS.iter().any(|e| eq_ignore_case(e, ext))
}

pub fn is_video_path(path: &str) -> bool {
    let ext = file_extension(path);
    VIDEO_EXTENSIONS.iter().any(|e| eq_ignore_case(e, ext))
}

/// The text shown for a mention element (user-mention etc.): the server keeps the original text (e.g. @all); wildcard mentions are shown by [`mention::display`]
/// as @everyone / @topic participants (Chinese labels); other mentions are returned as is.
pub fn mention_display(e: ElementRef) -> String {
    let text = text_of(e);
    if e.value().attr("data-user-id") != Some("*") && !has_class(e, "topic-mention") {
        return text;
    }
    mention::display(text.trim_start_matches('@'), !text.starts_with('@'), false)
}

/// Converts a class like `emoji-1f604` or `emoji-1f469-200d-1f4bb` to Unicode characters; returns `None` if it cannot be parsed.
pub fn emoji_from_class(class_name: &str) -> Option<String> {
    let codes = class_name
        .split_whitespace()
        .find_map(|c| c.strip_prefix("emoji-"))?;
    codes
        .split('-')
        .map(|hex| {
            if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            char::from_u32(u32::from_str_radix(hex, 16).ok()?)
        })
        .collect()
}

/// The URI scheme at the start (`letter *(letter / digit / + / - / .) :`).
fn has_uri_scheme(text: &str) -> bool {
    text.split_once(':').is_some_and(|(scheme, _)| {
        scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    })
}

/// Addresses allowed for bare images: http(s), or a path inside this organization (`/...`, `user_uploads/...`).
/// A path that still has a scheme after its leading `/` is removed does not count (the leading `/` is stripped when requesting, which would point the address to another site);
/// control characters are ignored when the address is parsed and can likewise change its meaning.
pub fn is_allowed_image_url(url: &str) -> bool {
    if starts_with_ignore_case(url, "http://") || starts_with_ignore_case(url, "https://") {
        return true;
    }
    if url.contains(|c: char| c == '\\' || c.is_ascii_control()) {
        return false;
    }
    match url.strip_prefix('/') {
        Some(path) => !path.starts_with('/') && !has_uri_scheme(path),
        None => url.starts_with(&UPLOADS_PREFIX[1..]),
    }
}

/// The URL of the outer link.
fn enclosing_href<'a>(e: ElementRef<'a>) -> Option<&'a str> {
    e.ancestors()
        .filter_map(ElementRef::wrap)
        .filter(|a| is_tag(*a, "a"))
        .find_map(|a| a.value().attr("href"))
}

/// Display URL of a bare image: while the thumbnail is not generated yet, src is a loading animation, so the original image is used. `None` if the URL is not allowed.
fn bare_image_source(img: ElementRef) -> Option<String> {
    let (src, _) = inline_image_source(img);
    let upload = has_class(img, "image-loading-placeholder")
        .then(|| enclosing_href(img).and_then(normalize_upload_path))
        .flatten();
    upload
        .or_else(|| src.map(str::to_owned))
        .filter(|s| is_allowed_image_url(s))
}

/// Images without the `inline-image` and `message_inline_image` classes (e.g. an image wrapped in a highlighted span in an edit-history diff),
/// shown as images when the URL is allowed. Emoji do not count.
pub fn is_bare_image(e: ElementRef) -> bool {
    is_tag(e, "img")
        && !has_class(e, "inline-image")
        && !has_class(e, "emoji")
        && !has_class(e, INLINE_IMAGE)
        && !has_ancestor(e, |a| has_class(a, INLINE_IMAGE))
        && bare_image_source(e).is_some()
}

/// Inline image previews in the message (elements with `message_inline_image`, excluding videos),
/// body images (`img.inline-image`) and bare images ([`is_bare_image`]), in order of appearance.
/// `src` is the display thumbnail (the upload path while the thumbnail is not generated yet),
/// `href` is the original link.
pub fn extract_inline_images(html: &str) -> Vec<InlineImage> {
    let doc = parse_fragment(html);
    let previews = inline_preview_paths(doc.root_element());
    let mut result = Vec::new();
    for e in descendants(doc.root_element()) {
        if let Some(path) = quoted_image_path(e, &previews) {
            result.push(InlineImage {
                src: path.clone(),
                href: path,
            });
        } else if is_tag(e, "img") && has_class(e, "inline-image") {
            if has_ancestor(e, |a| has_class(a, INLINE_IMAGE)) {
                continue;
            }
            if let (Some(src), href) = inline_image_source(e)
                && !src.is_empty()
            {
                result.push(InlineImage {
                    src: src.to_owned(),
                    href: href.unwrap_or(src).to_owned(),
                });
            }
        } else if is_bare_image(e) {
            if let Some(src) = bare_image_source(e) {
                result.push(InlineImage {
                    src,
                    href: enclosing_href(e).unwrap_or("").to_owned(),
                });
            }
        } else if has_class(e, INLINE_IMAGE) && !has_class(e, INLINE_VIDEO) {
            let Some(img) = first_descendant(e, "img") else {
                continue;
            };
            let href = first_href(e).unwrap_or("");
            let upload = normalize_upload_path(href);
            let src = match upload {
                Some(path) if has_class(img, "image-loading-placeholder") => Some(path),
                _ => img.value().attr("src").map(str::to_owned),
            };
            if let Some(src) = src.filter(|s| !s.is_empty()) {
                result.push(InlineImage {
                    src,
                    href: href.to_owned(),
                });
            }
        }
    }
    result
}

/// Replacement text for an inline element; `None` means no special handling and the child nodes must be visited recursively.
/// Image links in `images` return an empty string inside a paragraph (the inline preview shows them).
fn special_text(e: ElementRef, images: &HashSet<String>) -> Option<String> {
    let tag = e.value().name();
    // A video preview also carries message_inline_image, so check for video first
    if has_class(e, INLINE_VIDEO) {
        return Some("[视频]".into());
    }
    if has_class(e, INLINE_IMAGE) {
        return Some("[图片]".into());
    }
    if has_class(e, "codehilite") || tag == "pre" {
        return Some("[代码]".into());
    }
    if has_class(e, "katex-display") || has_class(e, "katex") {
        return Some("[公式]".into());
    }
    if has_class(e, "spoiler-block") {
        return Some("[折叠]".into());
    }
    if tag == "img" {
        return Some(if has_class(e, "emoji") {
            e.value().attr("alt").unwrap_or("").to_owned()
        } else {
            "[图片]".to_owned()
        });
    }
    if tag == "span" && has_class(e, "emoji") {
        return Some(
            e.value()
                .attr("class")
                .and_then(emoji_from_class)
                .unwrap_or_else(|| text_of(e)),
        );
    }
    if tag == "span"
        && ["user-mention", "user-group-mention", "topic-mention"]
            .iter()
            .any(|c| has_class(e, c))
    {
        return Some(mention_display(e));
    }
    if tag == "time"
        && let Some(t) = e.value().attr("datetime").and_then(time::parse_datetime)
    {
        return Some(time::local_label(&t));
    }
    if tag == "a"
        && let Some(path) = e.value().attr("href").and_then(normalize_upload_path)
    {
        let text = text_of(e);
        if images.contains(&path) {
            return Some(if is_file_name_label(&text, &path) {
                String::new()
            } else {
                text
            });
        }
        // The server generated no preview: since the server treats it as neither image nor video, show it as a file (consistent with the file card in the chat)
        return Some(format!("[文件]{}", text.trim()));
    }
    None
}

// The bullet (U+2022) or number "1." of a list item (same as the display in the composer); None when not a list item
fn list_marker(e: ElementRef) -> Option<String> {
    if !is_tag(e, "li") {
        return None;
    }
    let Some(ol) = e
        .parent()
        .and_then(ElementRef::wrap)
        .filter(|p| is_tag(*p, "ol"))
    else {
        return Some("•".into());
    };
    let start: i64 = ol
        .value()
        .attr("start")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let index = ol
        .children()
        .filter_map(ElementRef::wrap)
        .filter(|c| is_tag(*c, "li"))
        .take_while(|c| *c != e)
        .count();
    Some(format!("{}.", start + index as i64))
}

fn append_preview(node: NodeRef<'_, Node>, out: &mut String, images: &HashSet<String>) {
    if let Node::Text(text) = node.value() {
        out.push_str(text);
        return;
    }
    let Some(e) = ElementRef::wrap(node) else {
        return;
    };
    if is_tag(e, "blockquote") {
        return;
    }
    // Cell texts joined into one line would not look like a table
    if is_tag(e, "table") {
        out.push_str(" [表格] ");
        return;
    }
    let block = BLOCK_TAGS.contains(&e.value().name());
    if block {
        out.push(' ');
    }
    if let Some(marker) = list_marker(e) {
        out.push_str(&marker);
        out.push(' ');
    }
    match special_text(e, images) {
        Some(special) => {
            if !special.is_empty() {
                out.push(' ');
                out.push_str(&special);
                out.push(' ');
            }
        }
        None => {
            for child in e.children() {
                append_preview(child, out, images);
            }
        }
    }
    if block {
        out.push(' ');
    }
}

fn flush_inline(inline: &mut String, lines: &mut Vec<String>) {
    let collapsed = SPACES.replace_all(inline, " ");
    let trimmed = collapsed.trim();
    if !trimmed.is_empty() {
        lines.extend(trimmed.split('\n').map(|l| l.trim().to_owned()));
    }
    inline.clear();
}

fn push_code_lines(code: &str, lines: &mut Vec<String>) {
    lines.extend(code.trim_end_matches('\n').split('\n').map(str::to_owned));
}

/// Generates multi-line text per block; each block is several lines.
fn append_plain_blocks(parent: ElementRef, lines: &mut Vec<String>, images: &HashSet<String>) {
    let mut inline = String::new();
    for node in parent.children() {
        if let Node::Text(text) = node.value() {
            inline.push_str(&text.replace('\n', " "));
            continue;
        }
        let Some(e) = ElementRef::wrap(node) else {
            continue;
        };
        let tag = e.value().name();
        match tag {
            "br" => {
                inline.push('\n');
                continue;
            }
            "blockquote" => {
                flush_inline(&mut inline, lines);
                let mut inner = Vec::new();
                append_plain_blocks(e, &mut inner, images);
                lines.extend(inner.into_iter().map(|l| format!("> {l}")));
                continue;
            }
            "pre" => {
                flush_inline(&mut inline, lines);
                push_code_lines(&text_of(e), lines);
                continue;
            }
            "ul" | "ol" => {
                flush_inline(&mut inline, lines);
                let items = e
                    .children()
                    .filter_map(ElementRef::wrap)
                    .filter(|c| is_tag(*c, "li"));
                for (n, li) in items.enumerate() {
                    let mut inner = Vec::new();
                    append_plain_blocks(li, &mut inner, images);
                    let prefix = if tag == "ol" {
                        format!("{}. ", n + 1)
                    } else {
                        "• ".to_owned()
                    };
                    for (i, line) in inner.into_iter().enumerate() {
                        lines.push(if i == 0 {
                            format!("{prefix}{line}")
                        } else {
                            format!("  {line}")
                        });
                    }
                }
                continue;
            }
            _ => {}
        }

        if has_class(e, "codehilite") {
            flush_inline(&mut inline, lines);
            push_code_lines(&text_of(first_descendant(e, "pre").unwrap_or(e)), lines);
        } else if has_class(e, INLINE_IMAGE) {
            // The preview of an uploaded file replaces the file-name link in the paragraph; the preview of an external image gets its address from the link in the paragraph
            if is_upload_preview(e) {
                flush_inline(&mut inline, lines);
                lines.extend(special_text(e, images));
            }
        } else if has_class(e, "katex") || has_class(e, "katex-display") {
            let tex = descendants(e).find(|d| {
                is_tag(*d, "annotation") && d.value().attr("encoding") == Some("application/x-tex")
            });
            match tex {
                Some(tex) => inline.push_str(&format!("$${}$$", text_of(tex))),
                None => inline.push_str("[公式]"),
            }
        } else if BLOCK_TAGS.contains(&tag) {
            flush_inline(&mut inline, lines);
            append_plain_blocks(e, lines, images);
        } else {
            append_plain_inline(e, &mut inline, images);
        }
    }
    flush_inline(&mut inline, lines);
}

fn append_plain_inline(e: ElementRef, out: &mut String, images: &HashSet<String>) {
    if is_tag(e, "a")
        && let Some(href) = e.value().attr("href")
    {
        let text = text_of(e);
        // In plain text a file keeps its file name to ease copying; images / videos with a preview show no file name and are replaced by the [image] / [video] label at the preview
        if let Some(path) = normalize_upload_path(href) {
            if !images.contains(&path) || !is_file_name_label(&text, &path) {
                out.push_str(&text);
            }
            return;
        }
        if !href.starts_with('#') && text != href {
            out.push_str(&format!("{text} ({href})"));
            return;
        }
    }
    if let Some(special) = special_text(e, images) {
        out.push_str(&special);
        return;
    }
    for child in e.children() {
        if let Node::Text(text) = child.value() {
            out.push_str(&text.replace('\n', " "));
        } else if let Some(element) = ElementRef::wrap(child) {
            if is_tag(element, "br") {
                out.push('\n');
            } else {
                append_plain_inline(element, out, images);
            }
        }
    }
}

/// Serializes a node, keeping tags and attributes.
pub(crate) fn node_html(node: NodeRef<'_, Node>) -> String {
    match node.value() {
        Node::Text(text) => escape_text(text),
        Node::Comment(comment) => format!("<!--{}-->", &**comment),
        _ => ElementRef::wrap(node).map(|e| e.html()).unwrap_or_default(),
    }
}

fn escape_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('\u{a0}', "&nbsp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The HTML of an element, with its content replaced by `text`.
pub(crate) fn element_with_text(e: ElementRef, text: &str) -> String {
    let name = e.value().name();
    let mut out = format!("<{name}");
    for (attr, value) in e.value().attrs() {
        let value = value
            .replace('&', "&amp;")
            .replace('\u{a0}', "&nbsp;")
            .replace('"', "&quot;");
        out.push_str(&format!(" {attr}=\"{value}\""));
    }
    out.push_str(&format!(">{}</{name}>", escape_text(text)));
    out
}

/// HTML escaping of text (including quotes and non-breaking spaces).
pub fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    encode_into(&mut out, text);
    out
}

/// Appends the result of [`encode`] to `out`.
pub fn encode_into(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '\u{a0}' => out.push_str("&nbsp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Local, TimeZone, Utc};
    use serde_json::Value;

    use super::*;

    const SAMPLES: &str = include_str!("../testdata/rendered_samples.json");

    fn samples() -> Value {
        serde_json::from_str(SAMPLES).unwrap()
    }

    fn sample(name: &str) -> String {
        samples()[name].as_str().unwrap().to_owned()
    }

    #[test]
    fn encode_escapes_markup_quotes_and_nbsp() {
        assert_eq!(
            encode("a<b>&\"'\u{a0}中"),
            "a&lt;b&gt;&amp;&quot;&#39;&nbsp;中"
        );
        let mut out = "x".to_owned();
        encode_into(&mut out, "<");
        assert_eq!(out, "x&lt;");
    }

    fn real_image() -> String {
        samples()["real_image"][0].as_str().unwrap().to_owned()
    }

    const DESCRIBED_IMAGE: &str = "<p>见 <a href=\"/user_uploads/2/ab/x/design.png\">架构图</a> 说明</p>\n<div class=\"message_inline_image\"><a href=\"/user_uploads/2/ab/x/design.png\"><img src=\"/user_uploads/thumbnail/2/ab/x/design.png/840x560.webp\"></a></div>";
    const UPLOADED_VIDEO: &str = "<p>录屏 <a href=\"/user_uploads/2/ac/x/v.mp4\">v.mp4</a></p>\n<div class=\"message_inline_image message_inline_video\"><a href=\"/user_uploads/2/ac/x/v.mp4\" title=\"v.mp4\"><video src=\"/user_uploads/2/ac/x/v.mp4\"></video></a></div>";
    const UPLOADED_IMAGE: &str = "<p><a href=\"/user_uploads/2/ac/x/a.png\">a.png</a></p>\n<div class=\"message_inline_image\"><a href=\"/user_uploads/2/ac/x/a.png\" title=\"a.png\"><img data-original-dimensions=\"320x200\" src=\"/user_uploads/thumbnail/2/ac/x/a.png/840x560.webp\"></a></div>";
    const INLINE_IMAGE_P: &str = "<p>看<img alt=\"a.png\" class=\"inline-image\" data-original-dimensions=\"320x200\" data-original-src=\"/user_uploads/2/ac/x/a.png\" src=\"/user_uploads/thumbnail/2/ac/x/a.png/840x560.webp\">图</p>";
    const GALLERY: &str = "<div class=\"message-thumbnail-gallery\"><span class=\"message_inline_image\"><a href=\"/user_uploads/2/ac/x/a.png\"><img src=\"/user_uploads/thumbnail/2/ac/x/a.png/840x560.webp\"></a></span></div>";

    #[test]
    fn preview_quote_shows_only_new_content() {
        assert_eq!(to_preview(&sample("quote")), "阿杰 说: 回复内容");
    }

    #[test]
    fn preview_wildcard_mention() {
        let cases = [
            (
                r#"<p><span class="user-mention" data-user-id="*">@all</span> 体检</p>"#,
                "@全体成员 体检",
            ),
            (
                r#"<p><span class="user-mention silent" data-user-id="*">everyone</span></p>"#,
                "全体成员",
            ),
            (
                r#"<p><span class="topic-mention">@topic</span></p>"#,
                "@话题参与者",
            ),
            (
                r#"<p><span class="user-mention" data-user-id="5">@老王</span></p>"#,
                "@老王",
            ),
        ];
        for (html, expected) in cases {
            assert_eq!(to_preview(html), expected);
        }
    }

    #[test]
    fn preview_samples() {
        for (name, expected) in [
            ("code", "[代码]"),
            ("emoji", "你好 😄 👍"),
            ("list", "• 一 • 二 1. 甲 2. 乙"),
            ("math", "[公式] 与 [公式] [公式]"),
            ("spoiler", "[折叠]"),
            ("mentions", "@全体成员 @话题参与者 @Members"),
            // The server generated no preview: since the server does not treat it as an image, show it as a file
            ("img", "[文件]a.png"),
            ("strong", "粗 斜 删 code 链接"),
        ] {
            assert_eq!(to_preview(&sample(name)), expected, "{name}");
        }
    }

    #[test]
    fn preview_time_is_local() {
        let expected = Utc
            .with_ymd_and_hms(2026, 10, 22, 2, 0, 0)
            .unwrap()
            .with_timezone(&Local)
            .format("%Y/%m/%d %H:%M")
            .to_string();
        assert_eq!(to_preview(&sample("time")), expected);
    }

    #[test]
    fn image_extension_without_preview_is_file() {
        let html = "<p>截图 <a href=\"/user_uploads/2/11/x/a.png\">a.png</a></p>";
        let uploads = extract_uploads(html);
        assert_eq!(uploads.len(), 1);
        assert!(!uploads[0].is_image);
        assert!(has_file_attachment(html));
        assert_eq!(to_preview(html), "截图 [文件]a.png");
    }

    #[test]
    fn preview_misc() {
        assert_eq!(to_preview(&real_image()), "截图 [图片]");
        assert_eq!(
            to_preview(
                "<p>给你发个压缩包 <a href=\"/user_uploads/2/85/w2m/资料.zip\">资料.zip</a></p>"
            ),
            "给你发个压缩包 [文件]资料.zip"
        );
        assert_eq!(to_preview(""), "");
        assert_eq!(to_preview(DESCRIBED_IMAGE), "见 架构图 说明 [图片]");
        assert_eq!(to_preview(UPLOADED_VIDEO), "录屏 [视频]");
        assert_eq!(
            to_preview(
                "<p>见下</p><table><thead><tr><th>a</th></tr></thead><tbody><tr><td>1&lt;br /&gt;2</td></tr></tbody></table>"
            ),
            "见下 [表格]"
        );
    }

    #[test]
    fn plain_text() {
        assert_eq!(
            to_plain_text(&sample("quote")),
            "阿杰 说:\n> 原文内容\n回复内容"
        );
        assert_eq!(to_plain_text(&sample("code")), "print(1)");
        assert_eq!(to_plain_text(&sample("list")), "• 一\n• 二\n1. 甲\n2. 乙");
        assert_eq!(to_plain_text(&real_image()), "截图 [图片]");
        assert_eq!(
            to_plain_text(
                "<p>给你发个压缩包 <a href=\"/user_uploads/2/85/w2m/资料.zip\">资料.zip</a></p>"
            ),
            "给你发个压缩包 资料.zip"
        );
        assert_eq!(to_plain_text(DESCRIBED_IMAGE), "见 架构图 说明\n[图片]");
        assert_eq!(to_plain_text(UPLOADED_VIDEO), "录屏\n[视频]");
        assert_eq!(
            to_plain_text(&sample("strong")),
            "粗 斜 删 code 链接 (https://example.com)"
        );
        assert_eq!(to_plain_text("<p>a<br>\nb</p>\n<p>c</p>"), "a\nb\nc");
    }

    #[test]
    fn images_in_quotes_join_the_album_in_document_order() {
        let html = r#"<blockquote><p>截图 <a href="/user_uploads/1/a/x.png">x.png</a> <a href="/user_uploads/1/a/y.zip">y.zip</a></p></blockquote><p><a href="/user_uploads/1/b/z.png">z.png</a></p>"#;
        let images = extract_inline_images(html);
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].src, "/user_uploads/1/a/x.png");
    }

    #[test]
    fn inlined_previews_text_with_image() {
        let doc = parse_fragment(&real_image());
        let pairs = inlined_previews(&doc);
        assert_eq!(pairs.len(), 1);
        assert!(is_tag(pairs[0].0, "a"));
        assert!(has_class(pairs[0].1, INLINE_IMAGE));
    }

    #[test]
    fn inlined_previews_exclusions() {
        for html in [UPLOADED_IMAGE, DESCRIBED_IMAGE, UPLOADED_VIDEO] {
            assert!(inlined_previews(&parse_fragment(html)).is_empty(), "{html}");
        }
    }

    #[test]
    fn file_name_labels() {
        for (text, expected) in [
            ("design.png", true),
            ("DESIGN.PNG", true),
            ("", true),
            ("user_uploads/2/ab/x/design.png", true),
            ("架构图", false),
        ] {
            assert_eq!(
                is_file_name_label(text, "/user_uploads/2/ab/x/design.png"),
                expected,
                "{text}"
            );
        }
    }

    #[test]
    fn extract_uploads_dedupes_and_detects_images() {
        let image = real_image();
        let uploads = extract_uploads(&image);
        assert_eq!(uploads.len(), 1);
        assert_eq!(
            uploads[0].path,
            "/user_uploads/2/ac/LxWueOIW7MbTvYUwx5Oq51ne/截图-开发.png"
        );
        assert_eq!(uploads[0].file_name, "截图-开发.png");
        assert!(uploads[0].is_image);
        assert!(!has_file_attachment(&image));
    }

    #[test]
    fn extract_uploads_rewritten_own_realm_url() {
        // When a full address of this organization is pasted, the server renders it as a relative link without the leading slash, and the link text keeps the full address
        let html = "<p><a href=\"user_uploads/2/ab/x/%E8%B5%84%E6%96%99.zip\">https://zulip.example.com/user_uploads/2/ab/x/%E8%B5%84%E6%96%99.zip</a></p>";
        let uploads = extract_uploads(html);
        assert_eq!(uploads.len(), 1);
        assert_eq!(
            uploads[0].path,
            "/user_uploads/2/ab/x/%E8%B5%84%E6%96%99.zip"
        );
        assert_eq!(uploads[0].file_name, "资料.zip");
        assert!(!uploads[0].is_image);
        assert!(has_file_attachment(html));
    }

    #[test]
    fn upload_link_keeps_extension() {
        for (text, expected) in [
            ("报告", "report.pdf"),
            ("季度报告.PDF", "季度报告.PDF"),
            ("", "report.pdf"),
        ] {
            assert_eq!(
                upload_link_of("/user_uploads/2/id/report.pdf", Some(text)).file_name,
                expected
            );
        }
    }

    #[test]
    fn other_server_is_ignored() {
        assert!(extract_uploads("<p><a href=\"https://other.example/user_uploads/2/ab/x/report.pdf\">report.pdf</a></p>").is_empty());
    }

    #[test]
    fn emoji_classes() {
        assert_eq!(emoji_from_class("emoji emoji-1f604").as_deref(), Some("😄"));
        assert_eq!(
            emoji_from_class("emoji emoji-1f469-200d-1f4bb").as_deref(),
            Some("👩‍💻")
        );
        assert_eq!(emoji_from_class("emoji emoji-zz"), None);
        assert_eq!(emoji_from_class("emoji"), None);
    }

    #[test]
    fn normalize_paths() {
        for (href, expected) in [
            ("/user_uploads/thumbnail/2/ac/x.png/840x560.webp", None),
            ("#narrow/near/1", None),
            ("https://example.com/user_uploads/2/a/b.txt", None),
            (
                "/user_uploads/2/a/b.txt?x=1",
                Some("/user_uploads/2/a/b.txt"),
            ),
            ("user_uploads/2/a/b.txt", Some("/user_uploads/2/a/b.txt")),
        ] {
            assert_eq!(normalize_upload_path(href).as_deref(), expected, "{href}");
        }
    }

    #[test]
    fn thumbnails_map_to_their_originals() {
        for (href, expected) in [
            (
                "/user_uploads/thumbnail/2/ac/x.png/840x560.webp",
                Some("/user_uploads/2/ac/x.png"),
            ),
            ("/user_uploads/2/a/b.txt", Some("/user_uploads/2/a/b.txt")),
            ("/user_uploads/thumbnail/x.webp", None),
            ("https://example.com/a.png", None),
        ] {
            assert_eq!(original_upload_path(href).as_deref(), expected, "{href}");
        }
    }

    #[test]
    fn attachment_only() {
        assert!(is_attachment_only(UPLOADED_IMAGE));
        assert!(is_attachment_only(&sample("img")));
        assert!(is_attachment_only(GALLERY));
        assert!(!is_attachment_only(&real_image()));
        assert!(!is_attachment_only(&sample("strong")));
        assert!(!is_attachment_only(
            "<div class=\"message_inline_image\"><a href=\"https://example.com/a.png\"><img src=\"/external_content/abc\"></a></div>"
        ));
        assert!(is_attachment_only(
            "<p><img class=\"inline-image\" data-original-src=\"/user_uploads/2/ac/x/a.png\" src=\"/user_uploads/thumbnail/2/ac/x/a.png/840x560.webp\"><br>\n<img class=\"inline-image\" data-original-src=\"/user_uploads/2/ac/x/b.png\" src=\"/user_uploads/thumbnail/2/ac/x/b.png/840x560.webp\"></p>"
        ));
        assert!(!is_attachment_only(INLINE_IMAGE_P));
    }

    #[test]
    fn extract_inline_images_uploaded_and_external() {
        let html = format!(
            "{UPLOADED_IMAGE}<div class=\"message_inline_image\"><a href=\"https://example.com/b.png\"><img src=\"/external_content/abc\"></a></div><div class=\"message_inline_image message_inline_video\"><a href=\"/user_uploads/2/ac/x/v.mp4\"><video src=\"/user_uploads/2/ac/x/v.mp4\"></video></a></div>"
        );
        let images = extract_inline_images(&html);
        assert_eq!(images.len(), 2);
        assert_eq!(
            images[0].src,
            "/user_uploads/thumbnail/2/ac/x/a.png/840x560.webp"
        );
        assert_eq!(images[0].original(), "/user_uploads/2/ac/x/a.png");
        assert_eq!(images[1].original(), "/external_content/abc");
    }

    #[test]
    fn extract_inline_images_includes_bare_images_in_order() {
        let html = concat!(
            r#"<p><span class="highlight_text_inserted"><img src="/user_uploads/1/a/x.png"></span>"#,
            r#"<a href="/user_uploads/1/a/o.png"><img src="/user_uploads/thumbnail/1/a/o.png/s.webp"></a>"#,
            r#"<img src="https://example.com/e.png"></p>"#,
        );
        let images = extract_inline_images(html);
        assert_eq!(
            images.iter().map(InlineImage::original).collect::<Vec<_>>(),
            [
                "/user_uploads/1/a/x.png",
                "/user_uploads/1/a/o.png",
                "https://example.com/e.png",
            ],
        );
    }

    #[test]
    fn bare_image_placeholder_uses_the_upload_link() {
        let html = r#"<a href="/user_uploads/1/a/o.png"><img class="image-loading-placeholder" src="/static/images/loading/loader-black.svg"></a>"#;
        assert_eq!(
            extract_inline_images(html)[0].src,
            "/user_uploads/1/a/o.png"
        );
    }

    #[test]
    fn image_url_rejects_paths_that_become_other_sites() {
        for url in [
            "/https://evil.test/a.png",
            "/HTTPS://evil.test/a.png",
            "/http:evil.test/a.png",
            "/javascript:alert(1)",
            "//evil.test/a.png",
            "///evil.test/a.png",
            "\\\\evil.test/a.png",
            "/\\evil.test/a.png",
            "/\thttps://evil.test/a.png",
            "/ht\ntps://evil.test/a.png",
            "/\t/evil.test/a.png",
        ] {
            assert!(!is_allowed_image_url(url), "{url:?}");
        }
        for url in [
            "https://z.test/user_uploads/a.png",
            "HTTP://z.test/a.png",
            "/user_uploads/1/a.png",
            "/user_uploads/1/a:b.png",
            "user_uploads/1/a.png",
            "/a/https://x.test/b.png",
            "/1:2.png",
        ] {
            assert!(is_allowed_image_url(url), "{url:?}");
        }
    }

    #[test]
    fn bare_image_rejects_other_schemes_and_skips_emoji() {
        for src in [
            "javascript:alert(1)",
            "JaVaScRiPt:alert(1)",
            "data:image/png;base64,AAAA",
            "file:///c:/a.png",
            "//evil.example/a.png",
            "/\\evil.example/a.png",
            "x.png",
            "",
        ] {
            let html = format!(r#"<p><img src="{src}"></p>"#);
            assert!(extract_inline_images(&html).is_empty(), "{src}");
        }
        assert!(extract_inline_images(r#"<img class="emoji" src="/e.png">"#).is_empty());
        assert!(extract_inline_images("<img>").is_empty());
    }

    #[test]
    fn extract_inline_images_any_tag_counts_as_preview() {
        let images = extract_inline_images(GALLERY);
        assert_eq!(images.len(), 1);
        assert_eq!(
            images[0].src,
            "/user_uploads/thumbnail/2/ac/x/a.png/840x560.webp"
        );
        assert_eq!(images[0].original(), "/user_uploads/2/ac/x/a.png");
        assert_eq!(to_preview(GALLERY), "[图片]");
        assert_eq!(to_plain_text(GALLERY), "[图片]");
        let uploads = extract_uploads(GALLERY);
        assert_eq!(uploads.len(), 1);
        assert!(uploads[0].is_image);
        let doc = parse_fragment(GALLERY);
        assert!(inline_preview_paths(doc.root_element()).contains("/user_uploads/2/ac/x/a.png"));
        let span = descendants(doc.root_element())
            .find(|e| is_tag(*e, "span"))
            .unwrap();
        assert!(is_upload_preview(span));
    }

    #[test]
    fn inlined_previews_with_span_preview() {
        let html = format!("<p>截图 <a href=\"/user_uploads/2/ac/x/a.png\">a.png</a></p>{GALLERY}");
        let doc = parse_fragment(&html);
        let pairs = inlined_previews(&doc);
        assert_eq!(pairs.len(), 1);
        assert!(is_tag(pairs[0].1, "span"));
    }

    #[test]
    fn inline_images_placeholder() {
        let html = "<div class=\"message_inline_image\"><a href=\"/user_uploads/2/ac/x/a.png\"><img class=\"image-loading-placeholder\" src=\"/static/images/loading/loader-black.svg\"></a></div>";
        assert_eq!(
            extract_inline_images(html)[0].src,
            "/user_uploads/2/ac/x/a.png"
        );
        let html = "<p><img class=\"inline-image image-loading-placeholder\" data-original-src=\"/user_uploads/2/ac/x/a.png\" src=\"/static/images/loading/loader-black.svg\"></p>";
        assert_eq!(
            extract_inline_images(html)[0].src,
            "/user_uploads/2/ac/x/a.png"
        );
    }

    #[test]
    fn inline_image_in_document_order() {
        let images = extract_inline_images(&format!("{INLINE_IMAGE_P}{UPLOADED_IMAGE}"));
        assert_eq!(images.len(), 2);
        assert_eq!(
            images[0].src,
            "/user_uploads/thumbnail/2/ac/x/a.png/840x560.webp"
        );
        assert_eq!(images[0].original(), "/user_uploads/2/ac/x/a.png");
    }

    #[test]
    fn inline_image_text_forms() {
        assert!(to_plain_text(INLINE_IMAGE_P).contains("[图片]"));
        assert!(to_preview(INLINE_IMAGE_P).contains("[图片]"));
    }
}
