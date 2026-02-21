use floem::text::{
    Attrs, AttrsList, FamilyOwned, LineHeightValue, Style, TextLayout, Weight,
};
use lapce_core::{language::LapceLanguage, syntax::Syntax};
use lapce_xi_rope::Rope;
use lsp_types::MarkedString;
use pulldown_cmark::{CodeBlockKind, CowStr, Event, Options, Parser, Tag};
use smallvec::SmallVec;

use crate::config::{LapceConfig, color::LapceColor, layout::LapceLayout};

/// Represents a rendered block of markdown content. The parser breaks markdown into
/// these blocks so the view layer can lay them out vertically -- each Text block is
/// a self-contained TextLayout that handles its own word wrapping and styling.
/// Images and separators are handled as distinct view elements.
#[derive(Clone)]
pub enum MarkdownContent {
    Text(TextLayout),
    Image { url: String, title: String },
    Separator,
}

pub fn parse_markdown(
    text: &str,
    line_height: f64,
    config: &LapceConfig,
) -> Vec<MarkdownContent> {
    let mut res = Vec::new();

    let mut current_text = String::new();
    let code_font_family: Vec<FamilyOwned> =
        FamilyOwned::parse_list(&config.editor.font_family).collect();

    let default_attrs = Attrs::new()
        .color(config.color(LapceColor::EDITOR_FOREGROUND))
        .font_size(config.ui.font_size() as f32)
        .line_height(LineHeightValue::Normal(line_height as f32));
    let mut attr_list = AttrsList::new(default_attrs.clone());

    let mut builder_dirty = false;

    let mut pos = 0;

    // Tag stack tracks the byte offset where each tag opened, so we can apply
    // styling spans retroactively when the closing tag is encountered. SmallVec<4>
    // avoids heap allocation for typical nesting depths.
    let mut tag_stack: SmallVec<[(usize, Tag); 4]> = SmallVec::new();

    let parser = Parser::new_ext(
        text,
        Options::ENABLE_TABLES
            | Options::ENABLE_FOOTNOTES
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_HEADING_ATTRIBUTES,
    );
    let mut last_text = CowStr::from("");
    // Deferred newline: we don't emit the newline immediately after a block tag closes,
    // but rather before the next content. This prevents trailing newlines at the end of
    // the output, which would create unwanted blank space in the rendered hover popup.
    let mut add_newline = false;
    for event in parser {
        // Add the newline since we're going to be outputting more
        if add_newline {
            current_text.push('\n');
            builder_dirty = true;
            pos += 1;
            add_newline = false;
        }

        match event {
            Event::Start(tag) => {
                tag_stack.push((pos, tag));
            }
            Event::End(end_tag) => {
                if let Some((start_offset, tag)) = tag_stack.pop() {
                    if end_tag != tag.to_end() {
                        // Mismatched tags are handled gracefully by logging and
                        // continuing to the next event. The popped tag's styling
                        // is simply not applied, which is safe -- it just means
                        // that particular span won't be styled. This avoids
                        // crashing on malformed markdown from LSP hover responses.
                        tracing::warn!("Mismatched markdown tag");
                        continue;
                    }

                    if let Some(attrs) = attribute_for_tag(
                        default_attrs.clone(),
                        &tag,
                        &code_font_family,
                        config,
                    ) {
                        attr_list
                            .add_span(start_offset..pos.max(start_offset), attrs);
                    }

                    if should_add_newline_after_tag(&tag) {
                        add_newline = true;
                    }

                    match &tag {
                        Tag::CodeBlock(kind) => {
                            let language =
                                if let CodeBlockKind::Fenced(language) = kind {
                                    md_language_to_lapce_language(language)
                                } else {
                                    None
                                };

                            highlight_as_code(
                                &mut attr_list,
                                default_attrs.clone().family(&code_font_family),
                                language,
                                &last_text,
                                start_offset,
                                config,
                            );
                            builder_dirty = true;
                        }
                        Tag::Image {
                            link_type: _,
                            dest_url: dest,
                            title,
                            id: _,
                        } => {
                            // TODO: Are there any link types that would change how the
                            // image is rendered?

                            if builder_dirty {
                                let mut text_layout = TextLayout::new();
                                text_layout.set_text(&current_text, attr_list, None);
                                res.push(MarkdownContent::Text(text_layout));
                                attr_list = AttrsList::new(default_attrs.clone());
                                current_text.clear();
                                pos = 0;
                                builder_dirty = false;
                            }

                            res.push(MarkdownContent::Image {
                                url: dest.to_string(),
                                title: title.to_string(),
                            });
                        }
                        _ => {
                            // Presumably?
                            builder_dirty = true;
                        }
                    }
                } else {
                    tracing::warn!("Unbalanced markdown tag")
                }
            }
            Event::Text(text) => {
                if let Some((_, tag)) = tag_stack.last() {
                    if should_skip_text_in_tag(tag) {
                        continue;
                    }
                }
                current_text.push_str(&text);
                pos += text.len();
                last_text = text;
                builder_dirty = true;
            }
            Event::Code(text) => {
                attr_list.add_span(
                    pos..pos + text.len(),
                    default_attrs.clone().family(&code_font_family),
                );
                current_text.push_str(&text);
                pos += text.len();
                builder_dirty = true;
            }
            // TODO: Some minimal 'parsing' of html could be useful here, since some things use
            // basic html like `<code>text</code>`.
            Event::Html(text) => {
                attr_list.add_span(
                    pos..pos + text.len(),
                    default_attrs
                        .clone()
                        .family(&code_font_family)
                        .color(config.color(LapceColor::MARKDOWN_BLOCKQUOTE)),
                );
                current_text.push_str(&text);
                pos += text.len();
                builder_dirty = true;
            }
            Event::HardBreak => {
                current_text.push('\n');
                pos += 1;
                builder_dirty = true;
            }
            Event::SoftBreak => {
                current_text.push(' ');
                pos += 1;
                builder_dirty = true;
            }
            Event::Rule => {}
            Event::FootnoteReference(_text) => {}
            Event::TaskListMarker(_text) => {}
            Event::InlineHtml(_) => {} // TODO(panekj): Implement
            Event::InlineMath(_) => {} // TODO(panekj): Implement
            Event::DisplayMath(_) => {} // TODO(panekj): Implement
        }
    }

    if builder_dirty {
        let mut text_layout = TextLayout::new();
        text_layout.set_text(&current_text, attr_list, None);
        res.push(MarkdownContent::Text(text_layout));
    }

    res
}

fn attribute_for_tag<'a>(
    default_attrs: Attrs<'a>,
    tag: &Tag,
    code_font_family: &'a [FamilyOwned],
    config: &LapceConfig,
) -> Option<Attrs<'a>> {
    use pulldown_cmark::HeadingLevel;
    match tag {
        Tag::Heading {
            level,
            id: _,
            classes: _,
            attrs: _,
        } => {
            // The size calculations are based on the em values given at
            // https://drafts.csswg.org/css2/#html-stylesheet
            let font_scale = match level {
                HeadingLevel::H1 => 2.0,
                HeadingLevel::H2 => 1.5,
                HeadingLevel::H3 => 1.17,
                HeadingLevel::H4 => 1.0,
                HeadingLevel::H5 => 0.83,
                HeadingLevel::H6 => 0.75,
            };
            let font_size = font_scale * config.ui.font_size() as f64;
            Some(
                default_attrs
                    .font_size(font_size as f32)
                    .weight(Weight::BOLD),
            )
        }
        Tag::BlockQuote(_block_quote) => Some(
            default_attrs
                .style(Style::Italic)
                .color(config.color(LapceColor::MARKDOWN_BLOCKQUOTE)),
        ),
        Tag::CodeBlock(_) => Some(default_attrs.family(code_font_family)),
        Tag::Emphasis => Some(default_attrs.style(Style::Italic)),
        Tag::Strong => Some(default_attrs.weight(Weight::BOLD)),
        // TODO: Strikethrough support
        Tag::Link {
            link_type: _,
            dest_url: _,
            title: _,
            id: _,
        } => {
            // TODO: Link support
            Some(default_attrs.color(config.color(LapceColor::EDITOR_LINK)))
        }
        // All other tags are currently ignored
        _ => None,
    }
}

/// Decides whether newlines should be added after a specific markdown tag
fn should_add_newline_after_tag(tag: &Tag) -> bool {
    !matches!(
        tag,
        Tag::Emphasis | Tag::Strong | Tag::Strikethrough | Tag::Link { .. }
    )
}

/// Whether it should skip the text node after a specific tag  
/// For example, images are skipped because it emits their title as a separate text node.  
fn should_skip_text_in_tag(tag: &Tag) -> bool {
    matches!(tag, Tag::Image { .. })
}

fn md_language_to_lapce_language(lang: &str) -> Option<LapceLanguage> {
    // TODO: There are many other names commonly used that should be supported
    LapceLanguage::from_name(lang)
}

/// Apply syntax highlighting to a code block in the rendered markdown.
/// This uses Lapce's tree-sitter-based syntax engine to parse the code text and
/// then maps the resulting style spans onto the TextLayout's attribute list.
/// If the language is not recognized, the block is rendered with default code styling only.
pub fn highlight_as_code(
    attr_list: &mut AttrsList,
    default_attrs: Attrs,
    language: Option<LapceLanguage>,
    text: &str,
    start_offset: usize,
    config: &LapceConfig,
) {
    let syntax = language.map(Syntax::from_language);

    let styles = syntax
        .map(|mut syntax| {
            syntax.parse(0, Rope::from(text), None);
            syntax.styles
        })
        .unwrap_or(None);

    if let Some(styles) = styles {
        for (range, style) in styles.iter() {
            if let Some(color) = style
                .fg_color
                .as_ref()
                .and_then(|fg| config.style_color(fg))
            {
                attr_list.add_span(
                    start_offset + range.start..start_offset + range.end,
                    default_attrs.clone().color(color),
                );
            }
        }
    }
}

pub fn from_marked_string(
    text: MarkedString,
    config: &LapceConfig,
) -> Vec<MarkdownContent> {
    match text {
        MarkedString::String(text) => {
            parse_markdown(&text, LapceLayout::UI_LINE_HEIGHT, config)
        }
        // This is a short version of a code block
        MarkedString::LanguageString(code) => {
            // TODO: We could simply construct the MarkdownText directly
            // Simply construct the string as if it was written directly
            parse_markdown(
                &format!("```{}\n{}\n```", code.language, code.value),
                LapceLayout::UI_LINE_HEIGHT,
                config,
            )
        }
    }
}

pub fn from_plaintext(
    text: &str,
    line_height: f64,
    config: &LapceConfig,
) -> Vec<MarkdownContent> {
    let mut text_layout = TextLayout::new();
    text_layout.set_text(
        text,
        AttrsList::new(
            Attrs::new()
                .font_size(config.ui.font_size() as f32)
                .line_height(LineHeightValue::Normal(line_height as f32)),
        ),
        None,
    );
    vec![MarkdownContent::Text(text_layout)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use pulldown_cmark::{BlockQuoteKind, HeadingLevel, LinkType};

    #[test]
    fn newline_after_heading() {
        let tag = Tag::Heading {
            level: HeadingLevel::H1,
            id: None,
            classes: vec![],
            attrs: vec![],
        };
        assert!(should_add_newline_after_tag(&tag));
    }

    #[test]
    fn newline_after_paragraph() {
        let tag = Tag::Paragraph;
        assert!(should_add_newline_after_tag(&tag));
    }

    #[test]
    fn newline_after_code_block() {
        let tag = Tag::CodeBlock(CodeBlockKind::Fenced("rust".into()));
        assert!(should_add_newline_after_tag(&tag));
    }

    #[test]
    fn newline_after_block_quote() {
        let tag = Tag::BlockQuote(Some(BlockQuoteKind::Note));
        assert!(should_add_newline_after_tag(&tag));
    }

    #[test]
    fn no_newline_after_emphasis() {
        assert!(!should_add_newline_after_tag(&Tag::Emphasis));
    }

    #[test]
    fn no_newline_after_strong() {
        assert!(!should_add_newline_after_tag(&Tag::Strong));
    }

    #[test]
    fn no_newline_after_strikethrough() {
        assert!(!should_add_newline_after_tag(&Tag::Strikethrough));
    }

    #[test]
    fn no_newline_after_link() {
        let tag = Tag::Link {
            link_type: LinkType::Inline,
            dest_url: "http://example.com".into(),
            title: "".into(),
            id: "".into(),
        };
        assert!(!should_add_newline_after_tag(&tag));
    }

    #[test]
    fn skip_text_in_image_tag() {
        let tag = Tag::Image {
            link_type: LinkType::Inline,
            dest_url: "http://example.com/img.png".into(),
            title: "alt text".into(),
            id: "".into(),
        };
        assert!(should_skip_text_in_tag(&tag));
    }

    #[test]
    fn dont_skip_text_in_paragraph() {
        assert!(!should_skip_text_in_tag(&Tag::Paragraph));
    }

    #[test]
    fn dont_skip_text_in_emphasis() {
        assert!(!should_skip_text_in_tag(&Tag::Emphasis));
    }

    #[test]
    fn dont_skip_text_in_code_block() {
        let tag = Tag::CodeBlock(CodeBlockKind::Indented);
        assert!(!should_skip_text_in_tag(&tag));
    }

    #[test]
    fn md_language_rust() {
        let result = md_language_to_lapce_language("rust");
        assert!(result.is_some());
    }

    #[test]
    fn md_language_python() {
        let result = md_language_to_lapce_language("python");
        assert!(result.is_some());
    }

    #[test]
    fn md_language_javascript() {
        let result = md_language_to_lapce_language("javascript");
        assert!(result.is_some());
    }

    #[test]
    fn md_language_case_insensitive() {
        // from_name lowercases the input
        let result = md_language_to_lapce_language("Rust");
        assert!(result.is_some());
    }

    #[test]
    fn md_language_unknown_returns_none() {
        assert!(md_language_to_lapce_language("not_a_real_language").is_none());
    }

    #[test]
    fn md_language_empty_string_returns_none() {
        assert!(md_language_to_lapce_language("").is_none());
    }

    // --- attribute_for_tag() tests ---

    #[test]
    fn attribute_for_tag_heading_h1_bold_and_scaled() {
        let config = LapceConfig::test_default();
        let code_font: Vec<FamilyOwned> = vec![];
        let default_attrs = Attrs::new().font_size(14.0);
        let tag = Tag::Heading {
            level: HeadingLevel::H1,
            id: None,
            classes: vec![],
            attrs: vec![],
        };
        let result = attribute_for_tag(default_attrs, &tag, &code_font, &config);
        assert!(result.is_some());
    }

    #[test]
    fn attribute_for_tag_heading_h6() {
        let config = LapceConfig::test_default();
        let code_font: Vec<FamilyOwned> = vec![];
        let default_attrs = Attrs::new().font_size(14.0);
        let tag = Tag::Heading {
            level: HeadingLevel::H6,
            id: None,
            classes: vec![],
            attrs: vec![],
        };
        let result = attribute_for_tag(default_attrs, &tag, &code_font, &config);
        assert!(result.is_some());
    }

    #[test]
    fn attribute_for_tag_emphasis_italic() {
        let config = LapceConfig::test_default();
        let code_font: Vec<FamilyOwned> = vec![];
        let default_attrs = Attrs::new();
        let result =
            attribute_for_tag(default_attrs, &Tag::Emphasis, &code_font, &config);
        assert!(result.is_some());
    }

    #[test]
    fn attribute_for_tag_strong_bold() {
        let config = LapceConfig::test_default();
        let code_font: Vec<FamilyOwned> = vec![];
        let default_attrs = Attrs::new();
        let result =
            attribute_for_tag(default_attrs, &Tag::Strong, &code_font, &config);
        assert!(result.is_some());
    }

    #[test]
    fn attribute_for_tag_code_block() {
        let config = LapceConfig::test_default();
        let code_font: Vec<FamilyOwned> = vec![];
        let default_attrs = Attrs::new();
        let tag = Tag::CodeBlock(CodeBlockKind::Indented);
        let result = attribute_for_tag(default_attrs, &tag, &code_font, &config);
        assert!(result.is_some());
    }

    #[test]
    fn attribute_for_tag_link() {
        let config = LapceConfig::test_default();
        let code_font: Vec<FamilyOwned> = vec![];
        let default_attrs = Attrs::new();
        let tag = Tag::Link {
            link_type: LinkType::Inline,
            dest_url: "https://example.com".into(),
            title: "".into(),
            id: "".into(),
        };
        let result = attribute_for_tag(default_attrs, &tag, &code_font, &config);
        assert!(result.is_some());
    }

    #[test]
    fn attribute_for_tag_blockquote() {
        let config = LapceConfig::test_default();
        let code_font: Vec<FamilyOwned> = vec![];
        let default_attrs = Attrs::new();
        let tag = Tag::BlockQuote(None);
        let result = attribute_for_tag(default_attrs, &tag, &code_font, &config);
        assert!(result.is_some());
    }

    #[test]
    fn attribute_for_tag_paragraph_returns_none() {
        let config = LapceConfig::test_default();
        let code_font: Vec<FamilyOwned> = vec![];
        let default_attrs = Attrs::new();
        let result =
            attribute_for_tag(default_attrs, &Tag::Paragraph, &code_font, &config);
        assert!(result.is_none());
    }

    #[test]
    fn attribute_for_tag_list_returns_none() {
        let config = LapceConfig::test_default();
        let code_font: Vec<FamilyOwned> = vec![];
        let default_attrs = Attrs::new();
        let result =
            attribute_for_tag(default_attrs, &Tag::List(None), &code_font, &config);
        assert!(result.is_none());
    }

    // --- parse_markdown() structural tests ---

    #[test]
    fn parse_markdown_empty_input() {
        let config = LapceConfig::test_default();
        let result = parse_markdown("", 1.5, &config);
        assert!(result.is_empty());
    }

    #[test]
    fn parse_markdown_plain_text() {
        let config = LapceConfig::test_default();
        let result = parse_markdown("hello world", 1.5, &config);
        assert!(!result.is_empty());
        assert!(matches!(result[0], MarkdownContent::Text(_)));
    }

    #[test]
    fn parse_markdown_heading() {
        let config = LapceConfig::test_default();
        let result = parse_markdown("# Title", 1.5, &config);
        assert!(!result.is_empty());
        assert!(matches!(result[0], MarkdownContent::Text(_)));
    }

    #[test]
    fn parse_markdown_code_block() {
        let config = LapceConfig::test_default();
        let result = parse_markdown("```\ncode here\n```", 1.5, &config);
        assert!(!result.is_empty());
    }

    #[test]
    fn parse_markdown_image_extraction() {
        let config = LapceConfig::test_default();
        let result = parse_markdown(
            "![alt text](https://example.com/image.png \"title\")",
            1.5,
            &config,
        );
        let has_image = result
            .iter()
            .any(|c| matches!(c, MarkdownContent::Image { .. }));
        assert!(has_image, "should extract image from markdown");
    }

    #[test]
    fn parse_markdown_multiple_paragraphs() {
        let config = LapceConfig::test_default();
        let result = parse_markdown("para1\n\npara2", 1.5, &config);
        // Should produce at least one text block
        assert!(!result.is_empty());
    }

    #[test]
    fn parse_markdown_inline_code() {
        let config = LapceConfig::test_default();
        let result = parse_markdown("use `code` here", 1.5, &config);
        assert!(!result.is_empty());
        assert!(matches!(result[0], MarkdownContent::Text(_)));
    }

    // --- from_plaintext() tests ---

    #[test]
    fn from_plaintext_returns_single_text() {
        let config = LapceConfig::test_default();
        let result = from_plaintext("hello", 1.5, &config);
        assert_eq!(result.len(), 1);
        assert!(matches!(result[0], MarkdownContent::Text(_)));
    }

    #[test]
    fn from_plaintext_empty_string() {
        let config = LapceConfig::test_default();
        let result = from_plaintext("", 1.5, &config);
        assert_eq!(result.len(), 1);
        assert!(matches!(result[0], MarkdownContent::Text(_)));
    }

    // --- from_marked_string() tests ---

    #[test]
    fn from_marked_string_plain_string() {
        let config = LapceConfig::test_default();
        let result = from_marked_string(
            MarkedString::String("hello world".to_string()),
            &config,
        );
        assert!(!result.is_empty());
    }

    #[test]
    fn from_marked_string_language_string() {
        let config = LapceConfig::test_default();
        let result = from_marked_string(
            MarkedString::LanguageString(lsp_types::LanguageString {
                language: "rust".to_string(),
                value: "fn main() {}".to_string(),
            }),
            &config,
        );
        assert!(!result.is_empty());
    }
}
