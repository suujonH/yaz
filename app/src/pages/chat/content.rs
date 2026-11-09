//! Rewrites the server-rendered message HTML into the content shown in a chat bubble.

use crate::ui::{IconName, icon_html};
use ego_tree::{NodeId, NodeRef};
use scraper::{ElementRef, Node};
use std::collections::{HashMap, HashSet};
use zulip_core::html::{
    InlineImage, UploadLink, emoji_from_class, encode, encode_into, extract_inline_images,
    inline_preview_paths, inlined_previews, is_allowed_image_url, is_bare_image,
    is_file_name_label, is_upload_only_paragraph, mention_display, normalize_upload_path,
    parse_fragment, quoted_image_path, upload_link_of,
};
use zulip_core::markdown::table::{TableAlign, table_from_cells};
use zulip_core::text::time;

const INLINE_IMAGE: &str = "message_inline_image";
const INLINE_VIDEO: &str = "message_inline_video";
const IMAGE_MAX: f64 = 240.0;
const IMAGE_DEFAULT: (f64, f64) = (240.0, 135.0);

/// Tags that are neither shown nor have their child nodes expanded.
const DROPPED: &[&str] = &[
    "script", "style", "iframe", "object", "embed", "link", "meta", "form", "input", "button",
    "svg", "math", "video", "audio", "base",
];
/// Tags kept as they are; for other tags only the child nodes are kept.
const KEPT: &[&str] = &[
    "p",
    "br",
    "hr",
    "ul",
    "ol",
    "li",
    "strong",
    "em",
    "b",
    "i",
    "s",
    "del",
    "code",
    "span",
    "div",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "a",
    "sub",
    "sup",
    "time",
    "small",
    "thead",
    "tbody",
    "tr",
    "th",
    "td",
    "blockquote",
];
const VOID: &[&str] = &["br", "hr"];
/// Elements and attributes kept for formula layout: KaTeX only uses these; `style` is filtered
/// separately by [`KATEX_STYLES`].
const KATEX_TAGS: &[(&str, &[&str])] = &[
    ("span", &["class"]),
    (
        "svg",
        &["width", "height", "viewBox", "preserveAspectRatio"],
    ),
    ("path", &["d"]),
    ("line", &["x1", "y1", "x2", "y2", "stroke-width"]),
];
/// Style properties kept for formula layout: only size, position and color, so no external
/// resources are loaded.
const KATEX_STYLES: &[&str] = &[
    "height",
    "width",
    "min-width",
    "max-width",
    "vertical-align",
    "top",
    "bottom",
    "left",
    "right",
    "margin-left",
    "margin-right",
    "margin-top",
    "margin-bottom",
    "padding-left",
    "padding-right",
    "margin",
    "border-width",
    "border-top-width",
    "border-bottom-width",
    "border-left-width",
    "border-right-width",
    "border-style",
    "border-right-style",
    "border-color",
    "color",
    "background-color",
    "text-shadow",
];
const KEPT_ATTRS: &[&str] = &[
    "class",
    "href",
    "title",
    "colspan",
    "rowspan",
    "start",
    "datetime",
    "data-user-id",
    "data-user-group-id",
];

#[derive(Clone, Debug, PartialEq)]
pub enum Segment {
    Html(String),
    File(UploadLink),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rendered {
    pub segments: Vec<Segment>,
    /// Image address that has no local file yet.
    pub pending: Vec<String>,
}

pub struct RenderOptions<'a> {
    /// Sender of a "/me" message.
    pub me: Option<&'a str>,
    /// Local address of a downloaded image.
    pub local_image: &'a dyn Fn(&str) -> Option<String>,
}

fn has_class(e: ElementRef, class: &str) -> bool {
    e.value().classes().any(|c| c == class)
}

/// Addresses allowed for links: http, https, mailto and same-site relative paths. Browsers ignore
/// whitespace and control characters when parsing addresses, so strip them before checking;
/// `//host` and `\host` actually point to other sites and do not count as relative paths.
fn is_allowed_href(href: &str) -> bool {
    let href: String = href
        .chars()
        .filter(|c| !c.is_ascii_whitespace() && !c.is_ascii_control())
        .collect();
    if href.starts_with("//") || href.contains('\\') {
        return false;
    }
    let scheme_end = href.find([':', '/', '?', '#']);
    match scheme_end.filter(|&end| href[end..].starts_with(':')) {
        Some(end) => ["http", "https", "mailto"]
            .iter()
            .any(|scheme| href[..end].eq_ignore_ascii_case(scheme)),
        None => true,
    }
}

fn is_preview(e: ElementRef) -> bool {
    has_class(e, INLINE_IMAGE) && !has_class(e, INLINE_VIDEO)
}

fn is_body_image(e: ElementRef) -> bool {
    e.value().name() == "img"
        && has_class(e, "inline-image")
        && !e
            .ancestors()
            .filter_map(ElementRef::wrap)
            .any(|a| has_class(a, INLINE_IMAGE))
}

fn first_image(e: ElementRef) -> Option<ElementRef> {
    if e.value().name() == "img" {
        return Some(e);
    }
    e.descendants()
        .filter_map(ElementRef::wrap)
        .find(|d| d.value().name() == "img")
}

/// Display size of the image box: scaled down proportionally within the maximum side length, 16:9
/// when there is no size info.
fn fit_image(dimensions: Option<&str>) -> (f64, f64) {
    let parsed = dimensions.and_then(|d| {
        let (w, h) = d.split_once('x')?;
        let (w, h): (f64, f64) = (w.parse().ok()?, h.parse().ok()?);
        (w > 0.0 && h > 0.0).then_some((w, h))
    });
    let Some((w, h)) = parsed else {
        return IMAGE_DEFAULT;
    };
    let scale = (IMAGE_MAX / w).min(IMAGE_MAX / h).min(1.0);
    ((w * scale).round().max(16.0), (h * scale).round().max(16.0))
}

/// Uploaded files in video previews and in paragraphs that contain only an upload link.
fn uploads_in(e: ElementRef) -> Vec<(String, String)> {
    e.descendants()
        .filter_map(ElementRef::wrap)
        .filter(|d| d.value().name() == "a")
        .filter_map(|a| {
            let path = normalize_upload_path(a.value().attr("href")?)?;
            Some((path, a.text().collect::<String>()))
        })
        .collect()
}

struct Walker<'a, 'o> {
    out: String,
    segments: Vec<Segment>,
    options: &'o RenderOptions<'o>,
    images: Vec<InlineImage>,
    image_index: HashMap<NodeId, usize>,
    /// Image shown at the file-name link: link node -> preview element.
    inlined_at: HashMap<NodeId, ElementRef<'a>>,
    moved: HashSet<NodeId>,
    previews: HashSet<String>,
    tables: usize,
    pending: Vec<String>,
}

impl<'a> Walker<'a, '_> {
    fn flush(&mut self) {
        if !self.out.trim().is_empty() {
            self.segments
                .push(Segment::Html(std::mem::take(&mut self.out)));
        }
        self.out.clear();
    }

    fn top_level(&mut self, node: NodeRef<'a, Node>) {
        let Some(e) = ElementRef::wrap(node) else {
            self.node(node);
            return;
        };
        if self.moved.contains(&e.id()) {
            return;
        }
        if has_class(e, INLINE_VIDEO) {
            for (path, text) in uploads_in(e) {
                self.file(&path, &text);
            }
            return;
        }
        if e.value().name() == "p" && is_upload_only_paragraph(e) {
            let links = uploads_in(e);
            let labels_only = links.iter().all(|(path, text)| {
                !self.previews.contains(path) || is_file_name_label(text, path)
            });
            if labels_only {
                for (path, text) in links {
                    if !self.previews.contains(&path) {
                        self.file(&path, &text);
                    }
                }
                return;
            }
        }
        self.node(node);
    }

    fn file(&mut self, path: &str, text: &str) {
        self.flush();
        self.segments
            .push(Segment::File(upload_link_of(path, Some(text))));
    }

    fn children(&mut self, node: NodeRef<'a, Node>) {
        for child in node.children() {
            self.node(child);
        }
    }

    fn node(&mut self, node: NodeRef<'a, Node>) {
        match node.value() {
            Node::Text(text) => encode_into(&mut self.out, text),
            Node::Element(_) => {
                if let Some(e) = ElementRef::wrap(node) {
                    self.element(e);
                }
            }
            _ => {}
        }
    }

    fn element(&mut self, e: ElementRef<'a>) {
        if self.moved.contains(&e.id()) {
            return;
        }
        let name = e.value().name();
        if DROPPED.contains(&name) {
            return;
        }
        // The quote header is merged into the quote card that follows
        if name == "p"
            && next_element(e).is_some_and(|n| n.value().name() == "blockquote")
            && quote_source(e).is_some()
        {
            return;
        }
        if let Some(&index) = self.image_index.get(&e.id()) {
            self.image(e, index, false);
            return;
        }
        if let Some(preview) = self.inlined_at.get(&e.id()).copied() {
            if let Some(&index) = self.image_index.get(&preview.id()) {
                self.image(preview, index, true);
            }
            return;
        }
        let classes = |c: &str| has_class(e, c);
        if classes("user-mention") || classes("user-group-mention") || classes("topic-mention") {
            self.mention(e);
        } else if classes("emoji") {
            self.emoji(e);
        } else if classes("katex-display") || classes("katex") {
            self.tex(e);
        } else if classes("codehilite") || name == "pre" {
            self.code_block(e);
        } else if name == "table" {
            self.table(e);
        } else if name == "blockquote" {
            self.quote(e);
        } else if classes("spoiler-block") {
            self.spoiler(e);
        } else if name == "a" && self.is_hidden_label(e) {
        } else if name == "img" {
            self.out.push_str("[图片]");
        } else if name == "time" {
            self.time(e);
        } else if KEPT.contains(&name) {
            self.tag(e);
        } else {
            self.children(*e);
        }
    }

    /// For an upload link with a preview, when the text is only the file name, the preview replaces
    /// it.
    fn is_hidden_label(&self, a: ElementRef) -> bool {
        a.value()
            .attr("href")
            .and_then(normalize_upload_path)
            .is_some_and(|path| {
                self.previews.contains(&path)
                    && is_file_name_label(&a.text().collect::<String>(), &path)
            })
    }

    fn open_tag(&mut self, e: ElementRef) {
        let name = e.value().name();
        self.out.push('<');
        self.out.push_str(name);
        for (attr, value) in e.value().attrs() {
            if KEPT_ATTRS.contains(&attr) && (attr != "href" || is_allowed_href(value)) {
                self.out.push(' ');
                self.out.push_str(attr);
                self.out.push_str("=\"");
                encode_into(&mut self.out, value);
                self.out.push('"');
            }
        }
        if matches!(name, "td" | "th")
            && let Some(align) = cell_align(e)
        {
            self.out.push_str(" style=\"text-align:");
            self.out.push_str(align);
            self.out.push('"');
        }
        self.out.push('>');
    }

    fn tag(&mut self, e: ElementRef<'a>) {
        let name = e.value().name();
        self.open_tag(e);
        if VOID.contains(&name) {
            return;
        }
        if matches!(name, "td" | "th") {
            self.cell(e);
        } else {
            self.children(*e);
        }
        self.out.push_str("</");
        self.out.push_str(name);
        self.out.push('>');
    }

    /// In table cell text, the `<br />` escaped by the server is restored to a line break.
    fn cell(&mut self, e: ElementRef<'a>) {
        for child in e.children() {
            if let Node::Text(text) = child.value() {
                let escaped = encode(text)
                    .replace("&lt;br /&gt;", "<br>")
                    .replace("&lt;br/&gt;", "<br>")
                    .replace("&lt;br&gt;", "<br>");
                self.out.push_str(&escaped);
            } else {
                self.node(child);
            }
        }
    }

    fn image(&mut self, e: ElementRef, index: usize, inline: bool) {
        let Some(image) = self.images.get(index).cloned() else {
            return;
        };
        let source_image = first_image(e);
        // Images in quotes are links with no size info: the size is determined by the image
        if source_image.is_none() {
            let source = self.image_source(&image.src);
            self.out.push_str(&format!(
                "<span class=\"img-preview quoted\" data-index=\"{index}\"><img {source} alt=\"\"></span>"
            ));
            return;
        }
        let (w, h) =
            fit_image(source_image.and_then(|img| img.value().attr("data-original-dimensions")));
        let block = is_preview(e) && !inline;
        let tag = if block { "div" } else { "span" };
        let class = if block {
            "img-preview block"
        } else {
            "img-preview"
        };
        let source = self.image_source(&image.src);
        self.out.push_str(&format!(
            "<{tag} class=\"{class}\" data-index=\"{index}\" style=\"width:{w}px;height:{h}px\"><img {source} alt=\"\"></{tag}>"
        ));
    }

    /// The address attribute of `img`: shown directly if already downloaded, otherwise filled in by
    /// the script once the download finishes.
    fn image_source(&mut self, url: &str) -> String {
        // A disallowed address is neither downloaded nor shown; the image box is left empty
        if !is_allowed_image_url(url) {
            return String::new();
        }
        let remote = encode(url);
        match (self.options.local_image)(url) {
            Some(local) => format!("data-remote=\"{remote}\" src=\"{}\"", encode(&local)),
            None => {
                if !self.pending.iter().any(|p| p == url) {
                    self.pending.push(url.to_owned());
                }
                format!("data-remote=\"{remote}\"")
            }
        }
    }

    fn mention(&mut self, e: ElementRef) {
        let display = mention_display(e);
        let silent = if has_class(e, "silent") {
            " silent"
        } else {
            ""
        };
        self.out
            .push_str(&format!("<span class=\"mention{silent}\""));
        for attr in ["data-user-id", "data-user-group-id"] {
            if let Some(value) = e.value().attr(attr) {
                self.out.push_str(&format!(" {attr}=\"{}\"", encode(value)));
            }
        }
        // The displayed text has been replaced with "all members" and the like; the wildcard name
        // is needed when inserting on click
        if has_class(e, "topic-mention") {
            self.out.push_str(" data-wildcard=\"topic\"");
        } else if e.value().attr("data-user-id") == Some("*") {
            self.out.push_str(" data-wildcard=\"all\"");
        }
        self.out.push('>');
        encode_into(&mut self.out, &display);
        self.out.push_str("</span>");
    }

    fn emoji(&mut self, e: ElementRef) {
        let title = e.value().attr("title").unwrap_or_default();
        if e.value().name() == "img" {
            if let Some(src) = e.value().attr("src") {
                let source = self.image_source(src);
                self.out.push_str(&format!(
                    "<img class=\"emoji-img\" {source} title=\"{}\" alt=\"\">",
                    encode(title)
                ));
            }
            return;
        }
        let unicode = e.value().classes().find_map(emoji_from_class);
        match unicode {
            Some(text) => {
                self.out.push_str(&format!(
                    "<span class=\"emoji-char\" title=\"{}\">{}</span>",
                    encode(title),
                    encode(&text)
                ));
            }
            None => encode_into(&mut self.out, &e.text().collect::<String>()),
        }
    }

    /// Global time is shown in the local time zone; if the server-supplied time cannot be parsed,
    /// it is shown as is.
    fn time(&mut self, e: ElementRef<'a>) {
        let datetime = e.value().attr("datetime");
        let Some(at) = datetime.and_then(time::parse_datetime) else {
            self.tag(e);
            return;
        };
        self.out.push_str(&format!(
            "<time class=\"global-time\" datetime=\"{}\" title=\"已换算为你的时区\">{}</time>",
            encode(datetime.unwrap_or_default()),
            encode(&time::local_label(&at))
        ));
    }

    /// Formula: show the KaTeX layout typeset by the server, with `data-tex` keeping the original
    /// TeX for copying; falls back to the original TeX when there is no layout.
    fn tex(&mut self, e: ElementRef) {
        let source = e
            .descendants()
            .filter_map(ElementRef::wrap)
            .find(|d| d.value().name() == "annotation")
            .map_or_else(
                || e.text().collect::<String>(),
                |a| a.text().collect::<String>(),
            );
        let display = has_class(e, "katex-display");
        let layout = e
            .descendants()
            .filter_map(ElementRef::wrap)
            .find(|d| has_class(*d, "katex-html"));
        if let Some(layout) = layout {
            if display {
                self.out.push_str("<span class=\"katex-display\">");
            }
            self.out.push_str("<span class=\"katex\" data-tex=\"");
            encode_into(&mut self.out, source.trim());
            self.out.push_str("\">");
            self.katex(*layout);
            self.out.push_str("</span>");
            if display {
                self.out.push_str("</span>");
            }
            return;
        }
        let (open, close) = if display {
            (
                "<div class=\"tex-block\"><code class=\"tex\">",
                "</code></div>",
            )
        } else {
            ("<code class=\"tex\">", "</code>")
        };
        self.out.push_str(open);
        encode_into(&mut self.out, source.trim());
        self.out.push_str(close);
    }

    /// Copy the KaTeX layout: keep only the elements and attributes in [`KATEX_TAGS`]; for other
    /// elements keep only the child nodes.
    fn katex(&mut self, node: NodeRef<Node>) {
        let Some(e) = ElementRef::wrap(node) else {
            if let Node::Text(text) = node.value() {
                encode_into(&mut self.out, text);
            }
            return;
        };
        let name = e.value().name();
        let Some((_, attrs)) = KATEX_TAGS.iter().find(|(tag, _)| *tag == name) else {
            for child in node.children() {
                self.katex(child);
            }
            return;
        };
        self.out.push('<');
        self.out.push_str(name);
        for (attr, value) in e.value().attrs() {
            let value = if attr == "style" {
                katex_style(value)
            } else if attrs.contains(&attr) {
                value.to_owned()
            } else {
                continue;
            };
            if !value.is_empty() {
                self.out
                    .push_str(&format!(" {attr}=\"{}\"", encode(&value)));
            }
        }
        self.out.push('>');
        for child in node.children() {
            self.katex(child);
        }
        self.out.push_str("</");
        self.out.push_str(name);
        self.out.push('>');
    }

    fn code_block(&mut self, e: ElementRef) {
        self.out.push_str("<pre class=\"code-block scroll\"");
        // When copying as Markdown, restore ```language
        if let Some(language) = code_language(e) {
            self.out
                .push_str(&format!(" data-code-language=\"{}\"", encode(language)));
        }
        self.out.push_str("><code>");
        let text: String = e.text().collect();
        encode_into(&mut self.out, text.strip_suffix('\n').unwrap_or(&text));
        self.out.push_str("</code></pre>");
    }

    fn table(&mut self, e: ElementRef<'a>) {
        let index = self.tables;
        self.tables += 1;
        let more = icon_html(IconName::MoreVertical, 16);
        self.out.push_str(&format!(
            "<div class=\"table-host\" data-table=\"{index}\"><div class=\"table-tools\"><div class=\"table-more\" title=\"更多操作\">{more}</div></div><div class=\"table-scroll scroll\"><table>"
        ));
        self.children(*e);
        self.out.push_str("</table></div></div>");
    }

    fn quote(&mut self, e: ElementRef<'a>) {
        let header = quote_header(e);
        match &header {
            Some(h) => self.out.push_str(&format!(
                "<blockquote class=\"quote\" data-href=\"{}\"><div class=\"quote-head\"><span class=\"quote-author\">{}</span><span class=\"quote-jump\" title=\"跳转到原消息\">↗</span></div>",
                encode(&h.href),
                encode(&h.author)
            )),
            None => self.out.push_str("<blockquote class=\"quote\">"),
        }
        for child in e.children() {
            if header.as_ref().is_some_and(|h| h.node == child.id()) {
                continue;
            }
            self.node(child);
        }
        self.out.push_str("</blockquote>");
    }

    fn spoiler(&mut self, e: ElementRef<'a>) {
        let child = |class: &str| {
            e.children()
                .filter_map(ElementRef::wrap)
                .find(|c| has_class(*c, class))
        };
        let label = child("spoiler-header")
            .map(|h| h.text().collect::<String>())
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "折叠".to_owned());
        self.out.push_str("<div class=\"spoiler\"><div class=\"spoiler-head\"><span class=\"spoiler-arrow\">▸</span>");
        encode_into(&mut self.out, &label);
        self.out.push_str("</div><div class=\"spoiler-body\">");
        if let Some(body) = child("spoiler-content") {
            self.children(*body);
        }
        self.out.push_str("</div></div>");
    }
}

fn cell_align(e: ElementRef) -> Option<&'static str> {
    let style = e.value().attr("style")?.replace(' ', "");
    ["left", "center", "right"]
        .into_iter()
        .find(|a| style.contains(&format!("text-align:{a}")))
}

struct QuoteHeader {
    node: NodeId,
    author: String,
    href: String,
}

/// Parts allowed in KaTeX inline styles: the property is in [`KATEX_STYLES`], the value contains
/// only numbers, units, colors and `calc(...)`; positioning only allows `relative`, so nothing can
/// overlay content outside the message.
fn katex_style(style: &str) -> String {
    style
        .split(';')
        .filter_map(|declaration| {
            let (property, value) = declaration.split_once(':')?;
            let (property, value) = (property.trim().to_ascii_lowercase(), value.trim());
            let safe = !value.is_empty()
                && value.chars().all(|c| {
                    c.is_ascii_alphanumeric()
                        || matches!(c, '.' | '-' | '+' | '#' | '%' | ' ' | ',' | '(' | ')')
                });
            let allowed = KATEX_STYLES.contains(&property.as_str())
                || (property == "position" && value == "relative");
            (allowed && safe).then(|| format!("{property}:{value}"))
        })
        .collect::<Vec<_>>()
        .join(";")
}

fn next_element(e: ElementRef) -> Option<ElementRef> {
    e.next_siblings().find_map(ElementRef::wrap)
}

fn previous_element(e: ElementRef) -> Option<ElementRef> {
    e.prev_siblings().find_map(ElementRef::wrap)
}

/// A paragraph like "@someone [said](...near...):": exactly one mention, one link to a message and
/// a colon; missing any of them makes it an ordinary paragraph.
fn quote_source(p: ElementRef) -> Option<QuoteHeader> {
    if p.value().name() != "p" {
        return None;
    }
    let elements: Vec<ElementRef> = p.children().filter_map(ElementRef::wrap).collect();
    let [mention, link] = elements.as_slice() else {
        return None;
    };
    let href = link.value().attr("href")?;
    if !has_class(*mention, "user-mention") || !href.contains("/near/") || !is_allowed_href(href) {
        return None;
    }
    let text: String = p
        .children()
        .filter_map(|n| n.value().as_text().map(|t| t.to_string()))
        .collect();
    if !matches!(text.trim(), ":" | "：") {
        return None;
    }
    Some(QuoteHeader {
        node: p.id(),
        author: mention_display(*mention).trim_start_matches('@').to_owned(),
        href: href.to_owned(),
    })
}

/// Header of a quote block: usually the paragraph right before the quote block, but it can also be
/// the first paragraph of the quote block.
fn quote_header(quote: ElementRef) -> Option<QuoteHeader> {
    previous_element(quote).and_then(quote_source).or_else(|| {
        quote
            .children()
            .find_map(ElementRef::wrap)
            .and_then(quote_source)
    })
}

/// Rewrite one message.
pub fn render(html: &str, options: &RenderOptions) -> Rendered {
    let doc = parse_fragment(html);
    let root = doc.root_element();
    let previews = inline_preview_paths(root);
    let mut image_index = HashMap::new();
    let mut next = 0;
    for e in root.descendants().filter_map(ElementRef::wrap) {
        if quoted_image_path(e, &previews).is_some()
            || is_body_image(e)
            || is_bare_image(e)
            || (is_preview(e) && first_image(e).is_some())
        {
            image_index.insert(e.id(), next);
            next += 1;
        }
    }
    let mut inlined_at = HashMap::new();
    let mut moved = HashSet::new();
    for (link, preview) in inlined_previews(&doc) {
        // When the preview element is already inside the paragraph, its link is inside it, so
        // nothing needs to move
        if link.ancestors().any(|a| a.id() == preview.id()) {
            continue;
        }
        inlined_at.insert(link.id(), preview);
        moved.insert(preview.id());
    }
    let mut walker = Walker {
        out: String::new(),
        segments: Vec::new(),
        options,
        images: extract_inline_images(html),
        image_index,
        inlined_at,
        moved,
        previews,
        tables: 0,
        pending: Vec::new(),
    };
    for child in root.children() {
        walker.top_level(child);
    }
    walker.flush();
    let mut segments = walker.segments;
    if let Some(name) = options.me
        && let Some(Segment::Html(first)) = segments.first_mut()
    {
        *first = first.replacen(
            "/me ",
            &format!("<span class=\"me-name\">{}</span> ", encode(name)),
            1,
        );
    }
    Rendered {
        segments,
        pending: walker.pending,
    }
}

/// Markdown source of the `index`-th table.
pub fn table_markdown(html: &str, index: usize, padded: bool) -> Option<String> {
    let doc = parse_fragment(html);
    let table = doc
        .root_element()
        .descendants()
        .filter_map(ElementRef::wrap)
        .filter(|e| e.value().name() == "table")
        .nth(index)?;
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut aligns = Vec::new();
    for row in table
        .descendants()
        .filter_map(ElementRef::wrap)
        .filter(|e| e.value().name() == "tr")
    {
        let cells: Vec<ElementRef> = row
            .children()
            .filter_map(ElementRef::wrap)
            .filter(|c| matches!(c.value().name(), "td" | "th"))
            .collect();
        if rows.is_empty() {
            aligns = cells
                .iter()
                .map(|c| match cell_align(*c) {
                    Some("center") => TableAlign::Center,
                    Some("right") => TableAlign::Right,
                    _ => TableAlign::Left,
                })
                .collect();
        }
        rows.push(
            cells
                .iter()
                .map(|c| {
                    let mut out = String::new();
                    inline_markdown(*c, &mut out);
                    out.replace("<br />", "\n").trim().to_owned()
                })
                .collect(),
        );
    }
    let borrowed: Vec<Vec<&str>> = rows
        .iter()
        .map(|r| r.iter().map(String::as_str).collect())
        .collect();
    Some(table_from_cells(&borrowed, &aligns, padded))
}

/// Markdown corresponding to the selected content (the rewritten HTML).
pub fn html_to_markdown(html: &str) -> String {
    let doc = parse_fragment(html);
    let mut blocks = Vec::new();
    collect_blocks(*doc.root_element(), &mut blocks);
    blocks.join("\n\n").trim().to_owned()
}

fn is_block(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "ul"
            | "ol"
            | "pre"
            | "blockquote"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "table"
            | "hr"
    )
}

fn collect_blocks(node: NodeRef<Node>, out: &mut Vec<String>) {
    let mut inline = String::new();
    for child in node.children() {
        let block = ElementRef::wrap(child).filter(|e| is_block(e.value().name()));
        let Some(e) = block else {
            inline_node(child, &mut inline);
            continue;
        };
        if !inline.trim().is_empty() {
            out.push(inline.trim().to_owned());
        }
        inline.clear();
        block_markdown(e, out);
    }
    if !inline.trim().is_empty() {
        out.push(inline.trim().to_owned());
    }
}

/// Language of a code block: the server writes it on `div.codehilite`, and after conversion it is
/// written on `pre`.
fn code_language<'a>(e: ElementRef<'a>) -> Option<&'a str> {
    e.value().attr("data-code-language").or_else(|| {
        e.parent()
            .and_then(ElementRef::wrap)
            .and_then(|p| p.value().attr("data-code-language"))
    })
}

fn block_markdown(e: ElementRef, out: &mut Vec<String>) {
    let name = e.value().name();
    match name {
        "pre" => {
            let text: String = e.text().collect();
            let language = code_language(e).unwrap_or_default();
            // The fence is longer than the longest run of backticks in the code, so a ``` inside
            // the code does not end the code block early
            let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
            let fence = "`".repeat(longest.max(2) + 1);
            out.push(format!(
                "{fence}{language}\n{}\n{fence}",
                text.trim_end_matches('\n')
            ));
        }
        "ul" | "ol" => {
            let start: usize = e
                .value()
                .attr("start")
                .and_then(|s| s.parse().ok())
                .unwrap_or(1);
            let mut lines = Vec::new();
            for (i, li) in e
                .children()
                .filter_map(ElementRef::wrap)
                .filter(|c| c.value().name() == "li")
                .enumerate()
            {
                let mut parts = Vec::new();
                collect_blocks(*li, &mut parts);
                let body = parts.join("\n").replace('\n', "\n  ");
                if name == "ol" {
                    lines.push(format!("{}. {body}", start + i));
                } else {
                    lines.push(format!("- {body}"));
                }
            }
            out.push(lines.join("\n"));
        }
        "blockquote" => {
            let mut parts = Vec::new();
            collect_blocks(*e, &mut parts);
            out.push(
                parts
                    .join("\n\n")
                    .lines()
                    .map(|l| format!("> {l}").trim_end().to_owned())
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            let level = name[1..].parse().unwrap_or(1);
            let mut text = String::new();
            inline_markdown(e, &mut text);
            out.push(format!("{} {}", "#".repeat(level), text.trim()));
        }
        "hr" => out.push("---".to_owned()),
        "table" => {
            if let Some(md) = table_markdown(&e.html(), 0, false) {
                out.push(md);
            }
        }
        _ if has_class(e, "table-tools") || has_class(e, "quote-head") => {}
        _ => collect_blocks(*e, out),
    }
}

fn inline_markdown(e: ElementRef, out: &mut String) {
    for child in e.children() {
        inline_node(child, out);
    }
}

fn inline_node(node: NodeRef<Node>, out: &mut String) {
    match node.value() {
        Node::Text(text) => out.push_str(text),
        Node::Element(_) => {
            let Some(e) = ElementRef::wrap(node) else {
                return;
            };
            let wrapped = |out: &mut String, mark: &str| {
                let mut inner = String::new();
                inline_markdown(e, &mut inner);
                out.push_str(mark);
                out.push_str(&inner);
                out.push_str(mark);
            };
            match e.value().name() {
                "strong" | "b" => wrapped(out, "**"),
                "em" | "i" => wrapped(out, "*"),
                "del" | "s" => wrapped(out, "~~"),
                "code" => {
                    let text: String = e.text().collect();
                    let mark = if has_class(e, "tex") { "$$" } else { "`" };
                    out.push_str(mark);
                    out.push_str(&text);
                    out.push_str(mark);
                }
                "br" => out.push_str("<br />"),
                "a" => {
                    let mut inner = String::new();
                    inline_markdown(e, &mut inner);
                    match e.value().attr("href") {
                        Some(href) if href != inner => {
                            out.push_str(&format!("[{inner}]({href})"));
                        }
                        _ => out.push_str(&inner),
                    }
                }
                "span" if has_class(e, "mention") => {
                    let text: String = e.text().collect();
                    let name = text.trim_start_matches('@');
                    match e.value().attr("data-user-id") {
                        Some(id) if id != "*" => out.push_str(&format!("@**{name}|{id}**")),
                        _ => out.push_str(&format!("@**{name}**")),
                    }
                }
                "span" if has_class(e, "katex") => {
                    let tex = e.value().attr("data-tex").unwrap_or_default();
                    out.push_str(&format!("$${tex}$$"));
                }
                "time" => match e.value().attr("datetime") {
                    Some(datetime) => out.push_str(&format!("<time:{datetime}>")),
                    None => inline_markdown(e, out),
                },
                _ if has_class(e, "img-preview") => out.push_str("[图片]"),
                _ => inline_markdown(e, out),
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(html: &str) -> Rendered {
        render(
            html,
            &RenderOptions {
                me: None,
                local_image: &|_| None,
            },
        )
    }

    fn html_of(html: &str) -> String {
        run(html)
            .segments
            .into_iter()
            .filter_map(|s| match s {
                Segment::Html(h) => Some(h),
                Segment::File(_) => None,
            })
            .collect()
    }

    #[test]
    fn href_allows_only_web_mail_and_site_relative_links() {
        for href in [
            "https://x.test/a",
            "HTTP://x.test/a",
            "mailto:a@x.test",
            "/user_uploads/1/a.png",
            "user_uploads/1/a.png",
            "#narrow/channel/1-a/topic/b",
            "?a=b:c",
            "/a:b",
            "a/b:c",
            "",
        ] {
            assert!(is_allowed_href(href), "{href}");
        }
        for href in [
            "javascript:alert(1)",
            "JaVaScRiPt:alert(1)",
            " javascript:alert(1)",
            "java\tscript:alert(1)",
            "java\nscript:alert(1)",
            "\u{1}javascript:alert(1)",
            "vbscript:x",
            "data:text/html,x",
            "file:///c:/a",
            "ftp://x.test",
            "//evil.test/a",
            "\\\\evil.test/a",
            "/\\evil.test/a",
        ] {
            assert!(!is_allowed_href(href), "{href:?}");
        }
    }

    #[test]
    fn links_keep_allowed_href_and_lose_others() {
        assert_eq!(
            html_of(
                r##"<p><a href="https://x.test/a" title="t">a</a> <a href="#narrow/x">b</a></p>"##
            ),
            r##"<p><a href="https://x.test/a" title="t">a</a> <a href="#narrow/x">b</a></p>"##
        );
        assert_eq!(
            html_of(r#"<p><a href="javascript:alert(1)" title="t">a</a></p>"#),
            r#"<p><a title="t">a</a></p>"#
        );
        assert_eq!(
            html_of(r#"<p><a href="java&#9;script:alert(1)">a</a></p>"#),
            "<p><a>a</a></p>"
        );
    }

    #[test]
    fn quote_header_ignores_disallowed_link() {
        let html = r#"<blockquote><p><span class="user-mention">@甲</span> <a href="javascript://near/1">说</a>：</p></blockquote>"#;
        assert!(!html_of(html).contains("data-href"));
        assert!(!html_of(html).contains("javascript"));
    }

    #[test]
    fn notice_html_applies_the_whitelist() {
        assert_eq!(
            html_of(
                r#"<p onclick="x()" style="color:red">公告 <a href="javascript:alert(1)">a</a><script>evil()</script></p><iframe src="x"></iframe><img src="x" onerror="y()">"#
            ),
            "<p>公告 <a>a</a></p>[图片]"
        );
    }

    #[test]
    fn notice_keeps_http_links_and_upload_paragraphs_become_file_cards() {
        let html = r#"<p><a href="https://x.test/a" target="_blank">a</a></p><p><a href="/user_uploads/1/a/f.zip">f.zip</a></p>"#;
        let result = run(html);
        assert_eq!(
            result.segments[0],
            Segment::Html(r#"<p><a href="https://x.test/a">a</a></p>"#.to_owned())
        );
        assert!(matches!(&result.segments[1], Segment::File(l) if l.file_name == "f.zip"));
        assert!(run("").segments.is_empty());
    }

    #[test]
    fn notice_images_custom_emoji_and_spoiler_render_like_messages() {
        let html = concat!(
            r#"<p>表情 <img class="emoji" src="/user_avatars/2/emoji/images/1.png" title="custom"></p>"#,
            r#"<div class="message_inline_image"><a href="/user_uploads/1/a/x.png"><img src="/user_uploads/1/a/x.png"></a></div>"#,
            r#"<div class="spoiler-block"><div class="spoiler-header"><p>标题</p></div><div class="spoiler-content"><p>秘密</p></div></div>"#,
        );
        let result = run(html);
        let out = html_of(html);
        assert!(out.contains(
            r#"<img class="emoji-img" data-remote="/user_avatars/2/emoji/images/1.png" title="custom" alt="">"#
        ));
        assert!(out.contains(r#"class="img-preview block" data-index="0""#));
        assert!(out.contains(r#"<div class="spoiler"><div class="spoiler-head">"#));
        assert!(!out.contains("[图片]"));
        assert_eq!(
            result.pending,
            [
                "/user_avatars/2/emoji/images/1.png",
                "/user_uploads/1/a/x.png"
            ]
        );
    }

    #[test]
    fn images_with_disallowed_addresses_are_never_requested() {
        for src in [
            "/https://evil.test/a.png",
            "/HTTP:evil.test/a.png",
            "//evil.test/a.png",
            "/\\evil.test/a.png",
            "javascript:alert(1)",
            "data:image/png;base64,AA",
            "file:///c:/a.png",
        ] {
            let src = encode(src);
            for html in [
                format!(r#"<p><img class="emoji" src="{src}"></p>"#),
                format!(r#"<p><img class="inline-image" src="{src}"></p>"#),
                format!(
                    r#"<div class="message_inline_image"><a href="/x"><img src="{src}"></a></div>"#
                ),
                format!(r#"<p><img src="{src}"></p>"#),
            ] {
                let result = run(&html);
                assert!(result.pending.is_empty(), "{html}");
                assert!(!html_of(&html).contains("evil"), "{html}");
                assert!(!html_of(&html).contains("src="), "{html}");
            }
        }
    }

    #[test]
    fn drops_scripts_and_event_attributes() {
        let out =
            html_of(r#"<p onclick="x()">a<script>evil()</script></p><iframe src="x"></iframe>"#);
        assert_eq!(out, "<p>a</p>");
    }

    #[test]
    fn inline_image_becomes_preview_with_fitted_size() {
        let html = r#"<div class="message_inline_image"><a href="/user_uploads/1/a/x.png"><img data-original-dimensions="960x480" src="/user_uploads/thumbnail/1/a/x.png/840x560.webp"></a></div>"#;
        let result = run(html);
        let Segment::Html(out) = &result.segments[0] else {
            panic!("应当是 HTML");
        };
        assert!(out.contains("img-preview block"));
        assert!(out.contains(r#"data-index="0""#));
        assert!(out.contains("width:240px;height:120px"));
        assert_eq!(
            result.pending,
            vec!["/user_uploads/thumbnail/1/a/x.png/840x560.webp"]
        );
    }

    #[test]
    fn any_element_with_inline_image_class_is_a_preview() {
        let html = r#"<span class="message_inline_image foo"><img src="/a.png"></span>"#;
        assert!(html_of(html).contains(r#"img-preview"#));
    }

    #[test]
    fn local_image_is_used_directly() {
        let result = render(
            r#"<p><img class="inline-image" src="/a.png"></p>"#,
            &RenderOptions {
                me: None,
                local_image: &|url| Some(format!("http://local{url}")),
            },
        );
        assert!(result.pending.is_empty());
        assert_eq!(
            result.segments,
            vec![Segment::Html(
                "<p><span class=\"img-preview\" data-index=\"0\" style=\"width:240px;height:135px\"><img data-remote=\"/a.png\" src=\"http://local/a.png\" alt=\"\"></span></p>".to_owned()
            )]
        );
    }

    #[test]
    fn bare_image_in_highlight_span_becomes_preview() {
        let html = r#"<p><span class="highlight_text_inserted"><img data-original-dimensions="480x240" onerror="x()" onload="y()" src="/user_uploads/1/a/x.png"></span></p>"#;
        let result = run(html);
        let Segment::Html(out) = &result.segments[0] else {
            panic!("应当是 HTML");
        };
        assert!(out.contains(r#"<span class="highlight_text_inserted"><span class="img-preview" data-index="0" style="width:240px;height:120px"><img data-remote="/user_uploads/1/a/x.png" alt=""></span></span>"#));
        assert!(!out.contains("onerror") && !out.contains("onload"));
        assert_eq!(result.pending, vec!["/user_uploads/1/a/x.png"]);
    }

    #[test]
    fn preview_indexes_follow_the_extracted_images() {
        let html = r#"<p><img src="/a.png"><img class="emoji" src="/e.png"><img class="inline-image" src="/b.png"><img src="javascript:x()"></p><div class="message_inline_image"><a href="/user_uploads/1/c.png"><img src="/c.png"></a></div>"#;
        let images = extract_inline_images(html);
        let out = html_of(html);
        for (index, image) in images.iter().enumerate() {
            assert!(out.contains(&format!(
                r#"data-index="{index}" style="width:240px;height:135px"><img data-remote="{}""#,
                image.src
            )));
        }
        assert_eq!(images.len(), 3);
    }

    #[test]
    fn disallowed_bare_image_is_only_a_label() {
        for src in ["javascript:alert(1)", "data:image/png;base64,AA", ""] {
            let out = html_of(&format!(r#"<p><img src="{src}" onerror="x()"></p>"#));
            assert_eq!(out, "<p>[图片]</p>");
        }
    }

    #[test]
    fn upload_only_paragraph_becomes_file_card() {
        let result = run(r#"<p>看</p><p><a href="/user_uploads/1/a/r.pdf">r.pdf</a></p><p>尾</p>"#);
        assert_eq!(result.segments.len(), 3);
        let Segment::File(link) = &result.segments[1] else {
            panic!("应当是文件");
        };
        assert_eq!(link.path, "/user_uploads/1/a/r.pdf");
        assert_eq!(link.file_name, "r.pdf");
    }

    #[test]
    fn file_name_link_is_replaced_by_its_preview() {
        let html = r#"<p><a href="/user_uploads/1/a/x.png">x.png</a></p><div class="message_inline_image"><a href="/user_uploads/1/a/x.png"><img src="/t.webp"></a></div>"#;
        let out = run(html);
        assert_eq!(out.segments.len(), 1);
        assert!(matches!(&out.segments[0], Segment::Html(h) if h.contains("img-preview")));
    }

    #[test]
    fn video_preview_becomes_file_card() {
        let html = r#"<div class="message_inline_image message_inline_video"><a href="/user_uploads/1/a/v.mp4"><video src="/x"></video></a></div>"#;
        let out = run(html);
        assert!(matches!(&out.segments[0], Segment::File(l) if l.file_name == "v.mp4"));
    }

    #[test]
    fn table_gets_host_tools_and_scroll() {
        let out = html_of(
            "<table><thead><tr><th style=\"text-align: center;\">a</th></tr></thead><tbody><tr><td>x&lt;br /&gt;y</td></tr></tbody></table>",
        );
        assert!(out.contains(r#"class="table-host" data-table="0""#));
        assert!(out.contains("table-more"));
        assert!(out.contains("table-scroll scroll"));
        assert!(out.contains(r#"<th style="text-align:center">a</th>"#));
        assert!(out.contains("<td>x<br>y</td>"));
    }

    #[test]
    fn code_block_keeps_plain_text() {
        let out = html_of(
            r#"<div class="codehilite"><pre><span></span><code><span class="k">fn</span> a&lt;b&gt;
</code></pre></div>"#,
        );
        assert_eq!(
            out,
            "<pre class=\"code-block scroll\"><code>fn a&lt;b&gt;</code></pre>"
        );
    }

    #[test]
    fn tex_keeps_katex_layout_and_source() {
        let out = html_of(
            r#"<span class="katex"><span class="katex-mathml"><math><semantics><annotation encoding="application/x-tex">x^2</annotation></semantics></math></span><span class="katex-html" aria-hidden="true"><span class="strut" style="height:0.8em;vertical-align:-0.1em;position:fixed;background:url(x)" onclick="a()">x</span><svg width="1em" viewBox="0 0 10 10" onload="a()"><path d="M0 0h1"></path></svg><b>2</b></span></span>"#,
        );
        assert_eq!(
            out,
            r#"<span class="katex" data-tex="x^2"><span class="katex-html"><span class="strut" style="height:0.8em;vertical-align:-0.1em">x</span><svg viewBox="0 0 10 10" width="1em"><path d="M0 0h1"></path></svg>2</span></span>"#
        );
        assert_eq!(html_to_markdown(&out), "$$x^2$$");
    }

    #[test]
    fn display_tex_wraps_katex() {
        let out = html_of(
            r#"<p><span class="katex-display"><span class="katex"><span class="katex-mathml"><math><semantics><annotation encoding="application/x-tex">\sum x</annotation></semantics></math></span><span class="katex-html">x</span></span></span></p>"#,
        );
        assert_eq!(
            out,
            r#"<p><span class="katex-display"><span class="katex" data-tex="\sum x"><span class="katex-html">x</span></span></span></p>"#
        );
    }

    #[test]
    fn tex_without_layout_falls_back_to_source() {
        let out = html_of(
            r#"<span class="katex"><span class="katex-mathml"><math><semantics><annotation encoding="application/x-tex">x^2</annotation></semantics></math></span></span>"#,
        );
        assert_eq!(out, "<code class=\"tex\">x^2</code>");
    }

    #[test]
    fn katex_style_keeps_only_layout_properties() {
        assert_eq!(
            katex_style("width:calc(100% - 0.2em); color:#ff0000;position:relative"),
            "width:calc(100% - 0.2em);color:#ff0000;position:relative"
        );
        assert_eq!(
            katex_style(
                "position:absolute;background-image:url(x);top:1em\"x;margin-right:0.05em;"
            ),
            "margin-right:0.05em"
        );
    }

    #[test]
    fn global_time_shows_local_time() {
        let out = html_of(
            r#"<p><time datetime="2026-11-07T07:00:00Z">2026-11-07T15:00:00+08:00</time> <time datetime="x">原文</time></p>"#,
        );
        let label = time::local_label(&time::parse_datetime("2026-11-07T07:00:00Z").unwrap());
        assert_eq!(
            out,
            format!(
                r#"<p><time class="global-time" datetime="2026-11-07T07:00:00Z" title="已换算为你的时区">{label}</time> <time datetime="x">原文</time></p>"#
            )
        );
        assert_eq!(
            html_to_markdown(&out),
            "<time:2026-11-07T07:00:00Z> <time:x>"
        );
    }

    #[test]
    fn spoiler_defaults_header() {
        let out = html_of(
            r#"<div class="spoiler-block"><div class="spoiler-header"></div><div class="spoiler-content" aria-hidden="true"><p>秘密</p></div></div>"#,
        );
        assert!(out.contains("折叠"));
        assert!(out.contains("<div class=\"spoiler-body\"><p>秘密</p></div>"));
    }

    #[test]
    fn real_quote_header_before_the_blockquote_is_merged() {
        let out = html_of(
            r##"<p><span class="user-mention silent" data-user-id="9">张三</span> <a href="https://z.test/#narrow/channel/1/topic/b/near/12">发言道</a>：</p>
<blockquote><p>内容</p></blockquote>"##,
        );
        assert!(out.contains(r##"<blockquote class="quote" data-href="https://z.test/#narrow/channel/1/topic/b/near/12">"##));
        assert!(out.contains("quote-author\">张三"));
        assert!(!out.contains("发言道"));
    }

    #[test]
    fn paragraph_without_colon_stays_before_the_quote() {
        let out = html_of(
            r##"<p><span class="user-mention" data-user-id="9">张三</span> <a href="#narrow/near/12">这条</a></p><blockquote><p>内容</p></blockquote>"##,
        );
        assert!(out.contains("这条"));
        assert!(!out.contains("data-href"));
    }

    #[test]
    fn image_links_in_quotes_show_the_image() {
        let html =
            r#"<blockquote><p>截图 <a href="/user_uploads/1/a/x.png">x.png</a></p></blockquote>"#;
        let out = html_of(html);
        assert!(out.contains(r#"<span class="img-preview quoted" data-index="0"><img data-remote="/user_uploads/1/a/x.png" alt=""></span>"#));
    }

    #[test]
    fn quote_header_is_merged() {
        let out = html_of(
            r##"<blockquote><p><span class="user-mention silent" data-user-id="9">张三</span> <a href="/#narrow/channel/1-a/topic/b/near/12">说</a>：</p><p>内容</p></blockquote>"##,
        );
        assert!(out.contains("quote-author\">张三"));
        assert!(out.contains("near/12"));
        assert!(out.ends_with("<p>内容</p></blockquote>"));
        assert!(!out.contains(">说<"));
    }

    #[test]
    fn emoji_class_becomes_unicode() {
        let out = html_of(
            r#"<p><span aria-label="smile" class="emoji emoji-1f604" role="img" title="smile">:smile:</span></p>"#,
        );
        assert!(out.contains("😄"));
        assert!(!out.contains(":smile:"));
    }

    #[test]
    fn me_message_replaces_prefix() {
        let result = render(
            "<p>/me 挥手</p>",
            &RenderOptions {
                me: Some("张三"),
                local_image: &|_| None,
            },
        );
        assert_eq!(
            result.segments,
            vec![Segment::Html(
                "<p><span class=\"me-name\">张三</span> 挥手</p>".to_owned()
            )]
        );
    }

    #[test]
    fn table_source_keeps_alignment_and_formatting() {
        let html = r#"<table><thead><tr><th>甲</th><th style="text-align: right;">乙</th></tr></thead><tbody><tr><td><strong>1</strong></td><td>2</td></tr></tbody></table>"#;
        let plain = table_markdown(html, 0, false).expect("表格存在");
        assert!(plain.contains("**1**"));
        assert!(plain.lines().nth(1).is_some_and(|l| l.contains(":")));
        assert!(table_markdown(html, 1, false).is_none());
    }

    #[test]
    fn large_table_source_keeps_every_row_and_column() {
        const ROWS: usize = 300;
        const COLUMNS: usize = 30;
        let head: String = (0..COLUMNS).map(|c| format!("<th>列{c}</th>")).collect();
        let body: String = (0..ROWS)
            .map(|r| {
                let cells: String = (0..COLUMNS).map(|c| format!("<td>{r}-{c}</td>")).collect();
                format!("<tr>{cells}</tr>")
            })
            .collect();
        let html = format!("<table><thead><tr>{head}</tr></thead><tbody>{body}</tbody></table>");
        for padded in [false, true] {
            let source = table_markdown(&html, 0, padded).expect("表格存在");
            let lines: Vec<&str> = source.lines().collect();
            assert_eq!(lines.len(), ROWS + 2, "表头、分隔行和全部数据行");
            assert!(lines.iter().all(|l| l.matches('|').count() == COLUMNS + 1));
            let last = lines[lines.len() - 1];
            assert!(last.contains(&format!("{}-0", ROWS - 1)));
            assert!(last.contains(&format!("{}-{}", ROWS - 1, COLUMNS - 1)));
        }
    }

    #[test]
    fn selection_converts_to_markdown() {
        let html = r#"<p>a <strong>b</strong> <code>c</code> <a href="http://x">d</a></p><ul><li>一</li><li>二</li></ul><blockquote class="quote"><p>q</p></blockquote><pre class="code-block"><code>e
f</code></pre>"#;
        assert_eq!(
            html_to_markdown(html),
            "a **b** `c` [d](http://x)\n\n- 一\n- 二\n\n> q\n\n```\ne\nf\n```"
        );
    }

    #[test]
    fn copied_code_block_keeps_language() {
        let html = html_of(
            r#"<div class="codehilite" data-code-language="Python"><pre><span></span><code>a = 1
</code></pre></div>"#,
        );
        assert!(html.contains(r#"data-code-language="Python""#));
        assert_eq!(html_to_markdown(&html), "```Python\na = 1\n```");
        let fenced = r#"<pre class="code-block"><code>x
```
y</code></pre>"#;
        assert_eq!(html_to_markdown(fenced), "````\nx\n```\ny\n````");
    }
}
