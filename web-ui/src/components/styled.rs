//! Shared styled-text renderer for chat messages and channel topics.
//!
//! `parse_format` produces colour/bold spans; `linkify_spans` carves URLs out
//! of plain-text fragments; `emotify_spans` rewrites known `:name:` tokens into
//! emote spans. Spans with `link = Some(url)` are wrapped in an `<a>`; emote
//! spans render as an inline `<img class="emote">` (with the `:name:` token kept
//! in a visually-hidden span for copy/paste + screen readers).

use leptos::prelude::*;

use crate::format::{self, StyledSpan};

/// Render already-parsed spans into views.
pub fn render_spans(spans: Vec<StyledSpan>) -> Vec<AnyView> {
    spans
        .into_iter()
        .map(|span| {
            let css = span.css();
            if let Some(name) = span.emote_name {
                let src = format!("/emotes/{name}.gif");
                let token = span.text; // ":name:"
                view! {
                    <img class="emote" src=src alt="" title=token.clone() />
                    <span class="emote-code">{token}</span>
                }
                .into_any()
            } else if let Some(url) = span.link {
                let style = if css.is_empty() { String::new() } else { css };
                view! {
                    <a
                        href=url
                        target="_blank"
                        rel="noopener noreferrer"
                        class="msg-link"
                        style=style
                    >{span.text}</a>
                }
                .into_any()
            } else if span.has_style() {
                view! { <span style=css>{span.text}</span> }.into_any()
            } else {
                view! { <span>{span.text}</span> }.into_any()
            }
        })
        .collect::<Vec<_>>()
}

/// Full message pipeline: parse → linkify → optional emotify → render.
pub fn render_message_text(text: &str, emotes_on: bool) -> Vec<AnyView> {
    let base = format::linkify_spans(format::parse_format(text));
    let spans = if emotes_on {
        format::emotify_spans(base)
    } else {
        base
    };
    render_spans(spans)
}

/// Topic pipeline: parse → linkify (no emote expansion — topics stay clean).
pub fn render_topic_text(text: &str) -> Vec<AnyView> {
    render_spans(format::linkify_spans(format::parse_format(text)))
}
