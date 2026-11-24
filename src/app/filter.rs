//! Shared text filter for tables and logs.

/// Text filter used by tables and logs: a case-insensitive substring, or a case-insensitive
/// regex. An invalid regex matches everything (and reports `error`) rather than hiding
/// every row while the user is still typing it.
#[derive(Default)]
pub(super) struct Matcher {
    key: (String, bool),
    kind: MatchKind,
    pub(super) error: Option<String>,
}

#[derive(Default)]
enum MatchKind {
    #[default]
    All,
    Text(String),
    Regex(regex::Regex),
}

impl Matcher {
    pub(super) fn update(&mut self, filter: &str, regex: bool) {
        if self.key.0 == filter && self.key.1 == regex {
            return;
        }
        self.key = (filter.to_owned(), regex);
        self.error = None;
        self.kind = if filter.is_empty() {
            MatchKind::All
        } else if regex {
            match regex::RegexBuilder::new(filter)
                .case_insensitive(true)
                .size_limit(1 << 20)
                .build()
            {
                Ok(re) => MatchKind::Regex(re),
                Err(e) => {
                    self.error = Some(e.to_string());
                    MatchKind::All
                }
            }
        } else {
            MatchKind::Text(filter.to_lowercase())
        };
    }

    /// True when a non-empty, valid filter narrows the results.
    pub(super) fn is_active(&self) -> bool {
        !matches!(self.kind, MatchKind::All)
    }

    /// Byte ranges of every match in `text` (empty when inactive or nothing matches).
    pub(super) fn find(&self, text: &str) -> Vec<std::ops::Range<usize>> {
        match &self.kind {
            MatchKind::All => Vec::new(),
            MatchKind::Text(needle) => {
                // Lowercasing can change byte lengths, so compare char by char on the original.
                let needle: Vec<char> = needle.chars().collect();
                let hay: Vec<(usize, char)> = text.char_indices().collect();
                let mut out = Vec::new();
                let mut i = 0;
                while i + needle.len() <= hay.len() {
                    let hit = needle
                        .iter()
                        .enumerate()
                        .all(|(k, n)| hay[i + k].1.to_lowercase().eq(std::iter::once(*n)));
                    if hit {
                        let start = hay[i].0;
                        let end = hay.get(i + needle.len()).map_or(text.len(), |h| h.0);
                        out.push(start..end);
                        i += needle.len().max(1);
                    } else {
                        i += 1;
                    }
                }
                out
            }
            MatchKind::Regex(re) => re
                .find_iter(text)
                .filter(|m| !m.is_empty())
                .map(|m| m.range())
                .collect(),
        }
    }

    pub(super) fn matches(&self, text: &str) -> bool {
        match &self.kind {
            MatchKind::All => true,
            MatchKind::Text(needle) => text.to_lowercase().contains(needle),
            MatchKind::Regex(re) => re.is_match(text),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matcher_text_and_regex() {
        let mut m = Matcher::default();
        assert!(m.matches("anything"));
        m.update("GET", false);
        assert!(m.matches("--> get /"));
        assert!(!m.matches("POST /"));
        m.update(r"\b(4|5)\d\d\b", true);
        assert!(m.matches("GET / 503 5ms"));
        assert!(!m.matches("GET / 200 5ms"));
        m.update("(unclosed", true);
        assert!(m.error.is_some());
        assert!(m.matches("shown while invalid"));
        m.update("", true);
        assert!(m.error.is_none());
    }

    #[test]
    fn matcher_find_ranges() {
        let mut m = Matcher::default();
        assert!(m.find("abc").is_empty());
        m.update("ab", false);
        assert_eq!(m.find("xAByab"), vec![1..3, 4..6]);
        m.update(r"\d+", true);
        assert_eq!(m.find("a12b3"), vec![1..3, 4..5]);
    }
}
