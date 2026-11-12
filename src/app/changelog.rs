//! The "What's new" window: CHANGELOG.md compiled into the app and rendered by release.

use iced::{
    Color, Element, Length, Size,
    widget::{column, container, row, scrollable, text},
};

use super::{Message, icons as icon, theme, update};

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

pub fn view<'a>(window: Size) -> Element<'a, Message> {
    let releases = releases();
    let mut col = column![].spacing(4);
    if releases.is_empty() {
        col = col.push(theme::faint("No release notes."));
    }
    for (i, r) in releases.iter().enumerate() {
        if i > 0 {
            col = col.push(theme::vspace(10.0));
            col = col.push(theme::hline());
            col = col.push(theme::vspace(8.0));
        }
        let mut head = row![
            text(format!("v{}", r.version))
                .size(16.0)
                .font(theme::SEMIBOLD)
                .color(theme::TEXT)
        ]
        .spacing(8)
        .align_y(iced::Center);
        if r.version == update::CURRENT {
            head = head.push(theme::badge("installed", theme::OK, false));
        }
        if let Some(d) = r.date {
            head = head.push(theme::fill_space());
            head = head.push(theme::faint(d));
        }
        col = col.push(head);
        for (heading, items) in &r.sections {
            col = col.push(theme::vspace(4.0));
            if !heading.is_empty() {
                col = col.push(theme::label_small(*heading).color(section_color(heading)));
            }
            for item in items {
                col = col.push(
                    row![
                        text(icon::DOT_OUTLINE)
                            .font(theme::ICONS)
                            .size(14.0)
                            .color(theme::TEXT_FAINT),
                        theme::txt(item.clone()).width(Length::Fill),
                    ]
                    .spacing(6),
                );
            }
        }
    }
    let width = (window.width - 120.0).clamp(420.0, 600.0);
    let height = (window.height - 120.0).clamp(300.0, 620.0);
    container(column![
        row![
            text("What's new")
                .size(15.0)
                .font(theme::SEMIBOLD)
                .color(theme::TEXT),
            theme::fill_space(),
            theme::icon_button(icon::X, "Close", Some(Message::ShowChangelog(false))),
        ]
        .align_y(iced::Center),
        theme::vspace(8.0),
        scrollable(container(col).padding(iced::Padding {
            right: 12.0,
            ..Default::default()
        }))
        .direction(theme::thin())
        .style(theme::scrollbar)
        .height(Length::Fill),
    ])
    .padding(16)
    .width(width)
    .height(height)
    .style(theme::dialog)
    .into()
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
