use floem::{
    Renderer, View, ViewId,
    peniko::{
        Color,
        kurbo::{Point, Rect, Size},
    },
    prop_extractor,
    reactive::create_effect,
    style::{FontFamily, FontSize, LineHeight, Style, TextColor},
    taffy::prelude::NodeId,
    text::{Attrs, AttrsList, FamilyOwned, TextLayout, Weight},
};

prop_extractor! {
    Extractor {
        color: TextColor,
        font_size: FontSize,
        font_family: FontFamily,
        line_height: LineHeight,
    }
}

/// State updates sent to the FocusText view via ViewId::update_state.
/// Each variant triggers a re-layout of the text with updated styling.
enum FocusTextState {
    Text(String),
    FocusColor(Color),
    /// Byte indices into the text where fuzzy match highlights should be drawn.
    /// These come from the nucleo fuzzy matcher and are rendered in bold + focus_color.
    FocusIndices(Vec<usize>),
    /// Pre-resolved syntax color spans: (start_byte, end_byte, color).
    /// Applied as base colors before focus highlights are overlaid on top.
    SyntaxColors(Vec<(usize, usize, Color)>),
}

pub fn focus_text(
    text: impl Fn() -> String + 'static,
    focus_indices: impl Fn() -> Vec<usize> + 'static,
    focus_color: impl Fn() -> Color + 'static,
) -> FocusText {
    let id = ViewId::new();

    create_effect(move |_| {
        let new_text = text();
        id.update_state(FocusTextState::Text(new_text));
    });

    create_effect(move |_| {
        let focus_color = focus_color();
        id.update_state(FocusTextState::FocusColor(focus_color));
    });

    create_effect(move |_| {
        let focus_indices = focus_indices();
        id.update_state(FocusTextState::FocusIndices(focus_indices));
    });

    FocusText {
        id,
        text: "".to_string(),
        text_layout: None,
        focus_color: Color::BLACK,
        focus_indices: Vec::new(),
        syntax_colors: Vec::new(),
        focus_highlight: None,
        text_node: None,
        available_text: None,
        available_width: None,
        available_text_layout: None,
        style: Default::default(),
    }
}

/// Like `focus_text`, but also accepts syntax highlighting color spans.
/// The `syntax_colors` closure returns `Vec<(start_byte, end_byte, Color)>` which
/// are applied as base text colors before the focus match highlighting is overlaid.
pub fn focus_text_with_syntax(
    text: impl Fn() -> String + 'static,
    focus_indices: impl Fn() -> Vec<usize> + 'static,
    focus_color: impl Fn() -> Color + 'static,
    syntax_colors: impl Fn() -> Vec<(usize, usize, Color)> + 'static,
) -> FocusText {
    let id = ViewId::new();

    create_effect(move |_| {
        let new_text = text();
        id.update_state(FocusTextState::Text(new_text));
    });

    create_effect(move |_| {
        let focus_color = focus_color();
        id.update_state(FocusTextState::FocusColor(focus_color));
    });

    create_effect(move |_| {
        let focus_indices = focus_indices();
        id.update_state(FocusTextState::FocusIndices(focus_indices));
    });

    create_effect(move |_| {
        let colors = syntax_colors();
        id.update_state(FocusTextState::SyntaxColors(colors));
    });

    FocusText {
        id,
        text: "".to_string(),
        text_layout: None,
        focus_color: Color::BLACK,
        focus_indices: Vec::new(),
        syntax_colors: Vec::new(),
        focus_highlight: None,
        text_node: None,
        available_text: None,
        available_width: None,
        available_text_layout: None,
        style: Default::default(),
    }
}

/// Like `focus_text_with_syntax`, but highlights focus indices with a background color
/// and specific text color instead of bold + syntax color.
pub fn focus_text_highlighted(
    text: impl Fn() -> String + 'static,
    focus_indices: impl Fn() -> Vec<usize> + 'static,
    focus_color: impl Fn() -> Color + 'static,
    syntax_colors: impl Fn() -> Vec<(usize, usize, Color)> + 'static,
    focus_text_color: Color,
    focus_bg_color: Color,
    row_height: f64,
) -> FocusText {
    let id = ViewId::new();

    create_effect(move |_| {
        let new_text = text();
        id.update_state(FocusTextState::Text(new_text));
    });

    create_effect(move |_| {
        let focus_color = focus_color();
        id.update_state(FocusTextState::FocusColor(focus_color));
    });

    create_effect(move |_| {
        let focus_indices = focus_indices();
        id.update_state(FocusTextState::FocusIndices(focus_indices));
    });

    create_effect(move |_| {
        let colors = syntax_colors();
        id.update_state(FocusTextState::SyntaxColors(colors));
    });

    FocusText {
        id,
        text: "".to_string(),
        text_layout: None,
        focus_color: Color::BLACK,
        focus_indices: Vec::new(),
        syntax_colors: Vec::new(),
        focus_highlight: Some((focus_text_color, focus_bg_color, row_height)),
        text_node: None,
        available_text: None,
        available_width: None,
        available_text_layout: None,
        style: Default::default(),
    }
}

/// A text view that highlights specific character positions (fuzzy match indices) in bold
/// and a distinct color. When the text overflows available width, it truncates with "..."
/// and maintains highlight positions within the truncated text.
/// Used in palette items, completion labels, and search results.
pub struct FocusText {
    id: ViewId,
    /// The full text content.
    text: String,
    /// Pre-computed TextLayout with highlight spans applied.
    text_layout: Option<TextLayout>,
    focus_color: Color,
    focus_indices: Vec<usize>,
    /// Syntax highlighting color spans: (start_byte, end_byte, color).
    syntax_colors: Vec<(usize, usize, Color)>,
    /// When set, focus indices use (text_color, bg_color, row_height) with background
    /// rectangles instead of bold + syntax/focus color.
    focus_highlight: Option<(Color, Color, f64)>,
    text_node: Option<NodeId>,
    /// Truncated version of text (with "..." suffix) when the full text exceeds available_width.
    available_text: Option<String>,
    /// The width at which truncation was computed. Cached to avoid re-computing on every layout.
    available_width: Option<f32>,
    /// TextLayout for the truncated text, used when the full text doesn't fit.
    available_text_layout: Option<TextLayout>,
    style: Extractor,
}

impl FocusText {
    /// Build an AttrsList with syntax colors as the base layer and focus highlights on top.
    fn build_attrs_list(
        &self,
        text: &str,
        attrs: Attrs,
        truncated: bool,
    ) -> AttrsList {
        let mut attrs_list = AttrsList::new(attrs.clone());
        let text_len = text.len();

        // Layer 1: syntax colors (base)
        for &(start, end, color) in &self.syntax_colors {
            if start >= text_len {
                continue;
            }
            let end = end.min(text_len);
            if start < end {
                attrs_list.add_span(start..end, attrs.clone().color(color));
            }
        }

        // Layer 2: focus highlights on top.
        for &i_start in &self.focus_indices {
            if truncated && i_start + 3 > text_len {
                break;
            }
            let i_end = self
                .text
                .char_indices()
                .find(|(i, _)| *i == i_start)
                .map(|(_, c)| c.len_utf8() + i_start);
            let i_end = if let Some(i_end) = i_end {
                i_end
            } else {
                continue;
            };
            if let Some((text_color, _, _)) = self.focus_highlight {
                // Highlight mode: use specified text color (bg painted in paint())
                attrs_list.add_span(i_start..i_end, attrs.clone().color(text_color));
            } else {
                // Default mode: bold + syntax color (or focus_color fallback)
                let syntax_color = self
                    .syntax_colors
                    .iter()
                    .find(|(s, e, _)| i_start >= *s && i_start < *e)
                    .map(|(_, _, c)| *c);
                let color = syntax_color.unwrap_or(self.focus_color);
                attrs_list.add_span(
                    i_start..i_end,
                    attrs.clone().color(color).weight(Weight::BOLD),
                );
            }
        }

        attrs_list
    }

    fn set_text_layout(&mut self) {
        let mut attrs =
            Attrs::new().color(self.style.color().unwrap_or(Color::BLACK));
        if let Some(font_size) = self.style.font_size() {
            attrs = attrs.font_size(font_size);
        }
        let font_family = self.style.font_family().as_ref().map(|font_family| {
            let family: Vec<FamilyOwned> =
                FamilyOwned::parse_list(font_family).collect();
            family
        });
        if let Some(font_family) = font_family.as_ref() {
            attrs = attrs.family(font_family);
        }
        if let Some(line_height) = self.style.line_height() {
            attrs = attrs.line_height(line_height);
        }

        let attrs_list = self.build_attrs_list(&self.text, attrs.clone(), false);
        let mut text_layout = TextLayout::new();
        text_layout.set_text(&self.text, attrs_list, None);
        self.text_layout = Some(text_layout);

        if let Some(new_text) = self.available_text.as_ref() {
            let mut attrs =
                Attrs::new().color(self.style.color().unwrap_or(Color::BLACK));
            if let Some(font_size) = self.style.font_size() {
                attrs = attrs.font_size(font_size);
            }
            let font_family = self.style.font_family().as_ref().map(|font_family| {
                let family: Vec<FamilyOwned> =
                    FamilyOwned::parse_list(font_family).collect();
                family
            });
            if let Some(font_family) = font_family.as_ref() {
                attrs = attrs.family(font_family);
            }

            let attrs_list = self.build_attrs_list(new_text, attrs, true);
            let mut text_layout = TextLayout::new();
            text_layout.set_text(new_text, attrs_list, None);
            self.available_text_layout = Some(text_layout);
        }
    }
}

impl View for FocusText {
    fn id(&self) -> ViewId {
        self.id
    }

    fn update(
        &mut self,
        _cx: &mut floem::context::UpdateCx,
        state: Box<dyn std::any::Any>,
    ) {
        if let Ok(state) = state.downcast() {
            match *state {
                FocusTextState::Text(text) => {
                    self.text = text;
                }
                FocusTextState::FocusColor(color) => {
                    self.focus_color = color;
                }
                FocusTextState::FocusIndices(indices) => {
                    self.focus_indices = indices;
                }
                FocusTextState::SyntaxColors(colors) => {
                    self.syntax_colors = colors;
                }
            }
            self.set_text_layout();
            self.id.request_layout();
        }
    }

    fn style_pass(&mut self, cx: &mut floem::context::StyleCx<'_>) {
        if self.style.read(cx) {
            self.set_text_layout();
            self.id.request_layout();
        }
    }

    fn layout(
        &mut self,
        cx: &mut floem::context::LayoutCx,
    ) -> floem::taffy::prelude::NodeId {
        cx.layout_node(self.id, true, |_cx| {
            if self.text_layout.is_none() {
                self.set_text_layout();
            }

            let text_layout = self.text_layout.as_ref().unwrap();
            let size = text_layout.size();
            let width = size.width.ceil() as f32;
            let height = size.height as f32;

            if self.text_node.is_none() {
                self.text_node = Some(self.id.new_taffy_node());
            }
            let text_node = self.text_node.unwrap();

            let style = Style::new().width(width).height(height).to_taffy_style();
            self.id.set_taffy_style(text_node, style);
            vec![text_node]
        })
    }

    fn compute_layout(
        &mut self,
        _cx: &mut floem::context::ComputeLayoutCx,
    ) -> Option<Rect> {
        let text_node = self.text_node.unwrap();
        let layout = self.id.taffy_layout(text_node).unwrap_or_default();
        let text_layout = self.text_layout.as_ref().unwrap();
        let width = text_layout.size().width as f32;
        if width > layout.size.width {
            if self.available_width != Some(layout.size.width) {
                let mut dots_text = TextLayout::new();
                let mut attrs = Attrs::new().color(
                    self.style
                        .color()
                        .unwrap_or_else(|| Color::from_rgb8(0xf0, 0xf0, 0xea)),
                );
                if let Some(font_size) = self.style.font_size() {
                    attrs = attrs.font_size(font_size);
                }
                let font_family =
                    self.style.font_family().as_ref().map(|font_family| {
                        let family: Vec<FamilyOwned> =
                            FamilyOwned::parse_list(font_family).collect();
                        family
                    });
                if let Some(font_family) = font_family.as_ref() {
                    attrs = attrs.family(font_family);
                }
                dots_text.set_text("...", AttrsList::new(attrs), None);

                let dots_width = dots_text.size().width as f32;
                let width_left = layout.size.width - dots_width;
                let hit_point =
                    text_layout.hit_point(Point::new(width_left as f64, 0.0));
                let index = hit_point.index;

                let new_text = if index > 0 {
                    format!("{}...", &self.text[..index])
                } else {
                    "".to_string()
                };
                self.available_text = Some(new_text);
                self.available_width = Some(layout.size.width);
                self.set_text_layout();
            }
        } else {
            self.available_text = None;
            self.available_width = None;
            self.available_text_layout = None;
        }

        None
    }

    fn paint(&mut self, cx: &mut floem::context::PaintCx) {
        let text_node = self.text_node.unwrap();
        let location = self.id.taffy_layout(text_node).unwrap_or_default().location;
        let point = Point::new(location.x as f64, location.y as f64);
        let text_layout = if self.available_text_layout.is_some() {
            self.available_text_layout.as_ref().unwrap()
        } else {
            self.text_layout.as_ref().unwrap()
        };

        // Paint background rectangles for focus indices when highlight mode is active
        if let Some((_, bg_color, row_height)) = self.focus_highlight {
            let truncated = self.available_text_layout.is_some();
            let text_len = if truncated {
                self.available_text.as_ref().map_or(0, |t| t.len())
            } else {
                self.text.len()
            };
            let text_height = text_layout.size().height;
            let y_offset = -(row_height - text_height) / 2.0;

            // Group consecutive focus indices into contiguous byte ranges
            let mut ranges: Vec<(usize, usize)> = Vec::new();
            for &i_start in &self.focus_indices {
                if truncated && i_start + 3 > text_len {
                    break;
                }
                let i_end = self
                    .text
                    .char_indices()
                    .find(|(i, _)| *i == i_start)
                    .map(|(_, c)| c.len_utf8() + i_start);
                if let Some(i_end) = i_end {
                    if let Some(last) = ranges.last_mut() {
                        if i_start == last.1 {
                            last.1 = i_end;
                            continue;
                        }
                    }
                    ranges.push((i_start, i_end));
                }
            }

            for (range_start, range_end) in &ranges {
                let start_pos = text_layout.hit_position(*range_start);
                let end_pos = text_layout.hit_position(*range_end);
                let rect = Rect::ZERO
                    .with_size(Size::new(
                        end_pos.point.x - start_pos.point.x,
                        row_height,
                    ))
                    .with_origin(Point::new(start_pos.point.x + point.x, y_offset));
                cx.fill(&rect, bg_color, 0.0);
            }
        }

        cx.draw_text(text_layout, point);
    }
}
