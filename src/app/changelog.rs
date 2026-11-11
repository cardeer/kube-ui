//! The "What's new" window: CHANGELOG.md compiled into the app and rendered by release.

use eframe::egui::{self, Align2, RichText};
use egui_phosphor::regular as icon;

use super::{theme, update};

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
            out.push(Release { version: version.trim().trim_matches(['[', ']']), date, sections: Vec::new() });
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

/// Drops Markdown markers that egui would print literally.
fn clean(s: &str) -> String {
    s.replace("**", "").replace('`', "")
}

fn section_color(heading: &str) -> egui::Color32 {
    match heading {
        "Added" => theme::OK,
        "Fixed" => theme::ACCENT,
        "Removed" | "Security" => theme::BAD,
        "Changed" | "Deprecated" => theme::WARN,
        _ => theme::TEXT_MUTED,
    }
}

pub fn show(ctx: &egui::Context, open: &mut bool) {
    if !*open {
        return;
    }
    let releases = releases();
    egui::Window::new("What's new")
        .open(open)
        .collapsible(false)
        .resizable(true)
        .default_size([560.0, 520.0])
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                if releases.is_empty() {
                    ui.weak("No release notes.");
                }
                for (i, r) in releases.iter().enumerate() {
                    if i > 0 {
                        ui.add_space(14.0);
                        ui.separator();
                        ui.add_space(10.0);
                    }
                    ui.horizontal(|ui| {
                        ui.label(theme::semibold(format!("v{}", r.version)).size(16.0).color(theme::TEXT));
                        if r.version == update::CURRENT {
                            theme::badge(ui, "installed", theme::OK, false);
                        }
                        if let Some(d) = r.date {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.label(RichText::new(d).color(theme::TEXT_FAINT));
                            });
                        }
                    });
                    for (heading, items) in &r.sections {
                        ui.add_space(6.0);
                        if !heading.is_empty() {
                            ui.label(theme::label_small(*heading).color(section_color(heading)));
                            ui.add_space(2.0);
                        }
                        for item in items {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(RichText::new(icon::DOT_OUTLINE).color(theme::TEXT_FAINT));
                                ui.label(RichText::new(item).color(theme::TEXT));
                            });
                        }
                    }
                }
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bundled_changelog() {
        let r = releases();
        assert!(!r.is_empty());
        assert!(r.iter().all(|r| r.version != "Unreleased"));
        assert!(r.iter().any(|r| r.version == "0.2.1" && r.date == Some("2026-11-11")));
        let first = r.iter().find(|r| r.version == "0.1.0").unwrap();
        assert_eq!(first.sections[0], ("", vec!["First tagged release.".to_string()]));
        let added = &first.sections[1];
        assert_eq!(added.0, "Added");
        assert!(added.1[0].ends_with("network and port-forward views."));
    }
}
