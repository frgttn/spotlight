//! Markdown for detail views: parsed with `pulldown-cmark` into a few block
//! kinds, then drawn with GPUI text. Covers what descriptions need (headings,
//! paragraphs, lists, quotes, code, rules; bold, italic, inline code, links
//! shown as text), not all of CommonMark.

use std::ops::Range;

use gpui::{
    AnyElement, FontStyle, FontWeight, HighlightStyle, IntoElement, ParentElement, SharedString,
    Styled, StyledText, div, px, rgba,
};
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::theme;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Inline {
    pub text: String,
    pub styles: Vec<(Range<usize>, Span)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Span {
    Bold,
    Italic,
    Code,
    Link,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Heading(u8, Inline),
    Paragraph(Inline),
    /// Nesting depth (0 for a top-level item), the bullet ("•" or "3.").
    Item(usize, String, Inline),
    Quote(Inline),
    Code(String),
    Rule,
}

/// Parses `markdown` into blocks.
pub fn parse(markdown: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut current: Option<Inline> = None;
    // Open spans: kind and where they started in `current`.
    let mut open: Vec<(Span, usize)> = Vec::new();
    // Each open list: the next number, or `None` for bullets.
    let mut lists: Vec<Option<u64>> = Vec::new();
    let mut heading: Option<u8> = None;
    let mut in_quote = false;
    let mut code: Option<String> = None;
    let mut item_bullet: Option<String> = None;

    let finish = |blocks: &mut Vec<Block>,
                  current: &mut Option<Inline>,
                  heading: Option<u8>,
                  item: &mut Option<String>,
                  depth: usize,
                  in_quote: bool| {
        let Some(inline) = current.take() else {
            return;
        };
        if inline.text.trim().is_empty() && item.is_none() {
            return;
        }
        blocks.push(match (heading, item.take()) {
            (Some(level), _) => Block::Heading(level, inline),
            (None, Some(bullet)) => Block::Item(depth, bullet, inline),
            (None, None) if in_quote => Block::Quote(inline),
            (None, None) => Block::Paragraph(inline),
        });
    };

    for event in Parser::new_ext(markdown, Options::ENABLE_STRIKETHROUGH) {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                heading = Some(heading_level(level));
                current = Some(Inline::default());
            }
            Event::End(TagEnd::Heading(_)) => {
                finish(&mut blocks, &mut current, heading, &mut None, 0, false);
                heading = None;
            }
            Event::Start(Tag::Paragraph) => {
                // A tight list item's text arrives without a paragraph; a
                // loose one's inside one: keep the item's inline either way.
                if current.is_none() {
                    current = Some(Inline::default());
                }
            }
            Event::End(TagEnd::Paragraph) => {
                let depth = lists.len().saturating_sub(1);
                finish(
                    &mut blocks,
                    &mut current,
                    None,
                    &mut item_bullet,
                    depth,
                    in_quote,
                );
            }
            Event::Start(Tag::List(first)) => {
                // A nested list starts inside an item whose text came first.
                let depth = lists.len().saturating_sub(1);
                finish(
                    &mut blocks,
                    &mut current,
                    None,
                    &mut item_bullet,
                    depth,
                    in_quote,
                );
                lists.push(first);
            }
            Event::End(TagEnd::List(_)) => {
                lists.pop();
            }
            Event::Start(Tag::Item) => {
                let bullet = match lists.last_mut() {
                    Some(Some(number)) => {
                        let bullet = format!("{number}.");
                        *number += 1;
                        bullet
                    }
                    _ => "•".to_owned(),
                };
                item_bullet = Some(bullet);
                current = Some(Inline::default());
            }
            Event::End(TagEnd::Item) => {
                let depth = lists.len().saturating_sub(1);
                finish(
                    &mut blocks,
                    &mut current,
                    None,
                    &mut item_bullet,
                    depth,
                    in_quote,
                );
            }
            Event::Start(Tag::BlockQuote(_)) => in_quote = true,
            Event::End(TagEnd::BlockQuote(_)) => in_quote = false,
            Event::Start(Tag::CodeBlock(_)) => code = Some(String::new()),
            Event::End(TagEnd::CodeBlock) => {
                if let Some(text) = code.take() {
                    blocks.push(Block::Code(text.trim_end().to_owned()));
                }
            }
            Event::Start(Tag::Strong) => start_span(&current, &mut open, Span::Bold),
            Event::Start(Tag::Emphasis) => start_span(&current, &mut open, Span::Italic),
            Event::Start(Tag::Link { .. }) => start_span(&current, &mut open, Span::Link),
            Event::End(TagEnd::Strong | TagEnd::Emphasis | TagEnd::Link) => {
                if let (Some(inline), Some((span, start))) = (current.as_mut(), open.pop()) {
                    let end = inline.text.len();
                    if end > start {
                        inline.styles.push((start..end, span));
                    }
                }
            }
            Event::Code(text) => {
                if let Some(inline) = current.as_mut() {
                    let start = inline.text.len();
                    inline.text.push_str(&text);
                    inline.styles.push((start..inline.text.len(), Span::Code));
                }
            }
            Event::Text(text) => match (&mut code, current.as_mut()) {
                (Some(code), _) => code.push_str(&text),
                (None, Some(inline)) => inline.text.push_str(&text),
                (None, None) => {
                    current = Some(Inline {
                        text: text.into_string(),
                        styles: Vec::new(),
                    })
                }
            },
            Event::SoftBreak => {
                if let Some(inline) = current.as_mut() {
                    inline.text.push(' ');
                }
            }
            Event::HardBreak => {
                if let Some(inline) = current.as_mut() {
                    inline.text.push('\n');
                }
            }
            Event::Rule => blocks.push(Block::Rule),
            _ => {}
        }
    }
    let depth = lists.len().saturating_sub(1);
    finish(
        &mut blocks,
        &mut current,
        heading,
        &mut item_bullet,
        depth,
        in_quote,
    );
    blocks
}

fn start_span(current: &Option<Inline>, open: &mut Vec<(Span, usize)>, span: Span) {
    let at = current.as_ref().map_or(0, |inline| inline.text.len());
    open.push((span, at));
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// Draws `markdown` as a column of blocks.
pub fn render(markdown: &str) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .children(parse(markdown).into_iter().map(render_block))
        .into_any_element()
}

fn render_block(block: Block) -> AnyElement {
    match block {
        Block::Heading(level, inline) => div()
            .pt(px(if level <= 2 { 6. } else { 2. }))
            .text_size(px(match level {
                1 => 20.,
                2 => 16.,
                _ => 14.,
            }))
            .font_weight(FontWeight::SEMIBOLD)
            .child(styled(inline))
            .into_any_element(),
        Block::Paragraph(inline) => div()
            .text_color(rgba(theme::TEXT))
            .child(styled(inline))
            .into_any_element(),
        Block::Item(depth, bullet, inline) => div()
            .pl(px(4. + 16. * depth as f32))
            .flex()
            .gap_2()
            .child(
                div()
                    .flex_none()
                    .min_w(px(14.))
                    .text_color(rgba(theme::TEXT_MUTED))
                    .child(bullet),
            )
            .child(div().flex_1().min_w_0().child(styled(inline)))
            .into_any_element(),
        Block::Quote(inline) => div()
            .pl_3()
            .border_l_2()
            .border_color(rgba(theme::SURFACE_BORDER))
            .text_color(rgba(theme::TEXT_MUTED))
            .child(styled(inline))
            .into_any_element(),
        Block::Code(text) => div()
            .px_3()
            .py_2()
            .rounded(px(6.))
            .bg(rgba(theme::SURFACE))
            .font_family(MONOSPACE)
            .text_size(px(12.))
            .child(text)
            .into_any_element(),
        Block::Rule => div()
            .my_1()
            .h(px(1.))
            .bg(rgba(theme::HAIRLINE))
            .into_any_element(),
    }
}

/// Ships with every Windows since Vista.
const MONOSPACE: &str = "Consolas";

fn styled(inline: Inline) -> StyledText {
    let highlights: Vec<(Range<usize>, HighlightStyle)> = inline
        .styles
        .into_iter()
        .map(|(range, span)| {
            let style = match span {
                Span::Bold => HighlightStyle {
                    font_weight: Some(FontWeight::SEMIBOLD),
                    ..Default::default()
                },
                Span::Italic => HighlightStyle {
                    font_style: Some(FontStyle::Italic),
                    ..Default::default()
                },
                Span::Code => HighlightStyle {
                    background_color: Some(rgba(theme::SURFACE_HOVER).into()),
                    ..Default::default()
                },
                Span::Link => HighlightStyle {
                    color: Some(rgba(theme::ACCENT).into()),
                    ..Default::default()
                },
            };
            (range, style)
        })
        .collect();
    StyledText::new(SharedString::from(inline.text)).with_highlights(highlights)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(block: &Block) -> &str {
        match block {
            Block::Heading(_, inline)
            | Block::Paragraph(inline)
            | Block::Item(_, _, inline)
            | Block::Quote(inline) => &inline.text,
            Block::Code(code) => code,
            Block::Rule => "---",
        }
    }

    #[test]
    fn parses_blocks() {
        let blocks = parse(
            "# Title\n\nSome **bold** and *it* with `code`.\n\n- one\n- two\n  - nested\n\n1. first\n2. second\n\n> quoted\n\n```\nlet x = 1;\n```\n\n---\n",
        );
        let texts: Vec<_> = blocks.iter().map(text).collect();
        assert_eq!(
            texts,
            [
                "Title",
                "Some bold and it with code.",
                "one",
                "two",
                "nested",
                "first",
                "second",
                "quoted",
                "let x = 1;",
                "---"
            ]
        );
        assert!(matches!(blocks[0], Block::Heading(1, _)));
        assert!(matches!(&blocks[4], Block::Item(1, bullet, _) if bullet == "•"));
        assert!(matches!(&blocks[6], Block::Item(0, bullet, _) if bullet == "2."));
        assert!(matches!(blocks[7], Block::Quote(_)));
    }

    #[test]
    fn marks_inline_spans() {
        let blocks =
            parse("Press **Ctrl+K** for *more*, or see `--help` and [docs](https://x.dev).");
        let Block::Paragraph(inline) = &blocks[0] else {
            panic!("not a paragraph");
        };
        let spans: Vec<_> = inline
            .styles
            .iter()
            .map(|(range, span)| (&inline.text[range.clone()], *span))
            .collect();
        assert_eq!(
            spans,
            [
                ("Ctrl+K", Span::Bold),
                ("more", Span::Italic),
                ("--help", Span::Code),
                ("docs", Span::Link)
            ]
        );
    }
}
