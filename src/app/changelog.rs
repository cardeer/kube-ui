//! The "What's new" window: CHANGELOG.md compiled into the app and rendered by release.

use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::{
    AnyElement, FontWeight, InteractiveElement as _, IntoElement as _, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, div, px,
};

use super::{App, Message, Size, icons as icon, theme, theme::Color, update};

const SOURCE: &str = include_str!("../../CHANGELOG.md");

struct Release<'a> {
    version: &'a str,
    date: Option<&'a str>,
    /// (heading such as "Added", bullet items)
    sections: Vec<(&'a str, Vec<String>)>,
}

/// Released versions, newest first. "Unreleased" is left out: those changes ship once the
/// heading is renamed to the version being released.
fn releases() -> Vec<Release<'static>> {
    let mut out: Vec<Release> = Vec::new();
    for line in SOURCE.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            // "[0.2.1] - 2026-11-11"
            let (version, date) = match rest.split_once(" - ") {
                Some((v, d)) => (v, Some(d.trim())),
                None => (rest, None),
            };
            out.push(Release {
                version: version.trim().trim_matches(['[', ']']),
                date,
                sections: Vec::new(),
            });
        } else if let Some(heading) = line.strip_prefix("### ") {
            if let Some(r) = out.last_mut() {
                r.sections.push((heading.trim(), Vec::new()));
            }
        } else if let Some(item) = line.strip_prefix("- ") {
            if let Some(items) = section_items(&mut out) {
                items.push(clean(item));
            }
        } else if line.starts_with("  ") && !line.trim().is_empty() {
            // Continuation of a wrapped bullet.
            if let Some(last) = section_items(&mut out).and_then(|i| i.last_mut()) {
                last.push(' ');
                last.push_str(&clean(line.trim()));
            }
        } else if !line.trim().is_empty() && !line.starts_with('[') && !line.starts_with('#') {
            // A note under the release heading, e.g. "First tagged release."
            if let Some(r) = out.last_mut().filter(|r| r.sections.is_empty()) {
                r.sections.push(("", vec![clean(line.trim())]));
            }
        }
    }
    out.retain(|r| !r.version.eq_ignore_ascii_case("unreleased"));
    out
}

fn section_items<'a, 'b>(out: &'b mut [Release<'a>]) -> Option<&'b mut Vec<String>> {
    out.last_mut()?.sections.last_mut().map(|(_, items)| items)
}

/// Drops Markdown markers that would print literally.
fn clean(s: &str) -> String {
    s.replace("**", "").replace('`', "")
}

fn section_color(heading: &str) -> Color {
    match heading {
        "Added" => theme::OK,
        "Fixed" => theme::ACCENT,
        "Removed" | "Security" => theme::BAD,
        "Changed" | "Deprecated" => theme::WARN,
        _ => theme::TEXT_MUTED,
    }
}

/// The "What's new" window body, sized for the window.
pub fn view(window: Size) -> AnyElement {
    let releases = releases();
    let mut col = v_flex().gap(px(4.0)).w_full();
    if releases.is_empty() {
        col = col.child(theme::faint("No release notes."));
    }
    for (i, r) in releases.iter().enumerate() {
        if i > 0 {
            col = col
                .child(theme::vspace(10.0))
                .child(theme::hline())
                .child(theme::vspace(8.0));
        }
        let mut head = h_flex().items_center().gap(px(8.0)).child(
            div()
                .text_size(px(16.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::TEXT)
                .child(format!("v{}", r.version)),
        );
        if r.version == update::CURRENT {
            head = head.child(theme::badge("installed", theme::OK, false));
        }
        if let Some(d) = r.date {
            head = head.child(theme::fill_space()).child(theme::faint(d));
        }
        col = col.child(head);
        for (heading, items) in &r.sections {
            col = col.child(theme::vspace(4.0));
            if !heading.is_empty() {
                col = col.child(theme::label_small(*heading).text_color(section_color(heading)));
            }
            for item in items {
                col = col.child(
                    h_flex()
                        .items_start()
                        .gap(px(6.0))
                        .child(
                            theme::icon(icon::DOT_OUTLINE)
                                .text_color(theme::TEXT_FAINT)
                                .mt(px(2.0)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(theme::BODY))
                                .text_color(theme::TEXT)
                                .child(item.clone()),
                        ),
                );
            }
        }
    }
    let width = (window.width - 120.0).clamp(420.0, 600.0);
    let height = (window.height - 120.0).clamp(300.0, 620.0);
    theme::dialog()
        .p(px(16.0))
        .w(px(width))
        .h(px(height))
        .flex()
        .flex_col()
        .child(App::dialog_title("What's new", Message::ShowChangelog(false)))
        .child(theme::vspace(8.0))
        .child(
            div()
                .id("changelog-scroll")
                .flex_1()
                .min_h_0()
                .w_full()
                .overflow_y_scroll()
                .child(div().pr(px(12.0)).child(col)),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bundled_changelog() {
        let r = releases();
        assert!(!r.is_empty());
        assert!(r.iter().all(|r| r.version != "Unreleased"));
        assert!(
            r.iter()
                .any(|r| r.version == "0.2.1" && r.date == Some("2026-11-11"))
        );
        let first = r.iter().find(|r| r.version == "0.1.0").unwrap();
        assert_eq!(
            first.sections[0],
            ("", vec!["First tagged release.".to_string()])
        );
        let added = &first.sections[1];
        assert_eq!(added.0, "Added");
        assert!(added.1[0].ends_with("network and port-forward views."));
    }
}
