// SPDX-License-Identifier: MIT

//! Searching the scrollback for literal text.

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Boundary, Column, Direction, Line, Point, Side};
use alacritty_terminal::term::Term;
use alacritty_terminal::term::search::{Match, RegexIter, RegexSearch};

/// The most matches highlighted on one screen.
const MAX_VISIBLE_MATCHES: usize = 1000;

/// Which way to move from the current match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchDirection {
    /// Toward older output, up the scrollback.
    Older,
    /// Toward newer output.
    Newer,
}

/// The state of a scrollback search: the pattern and the focused match.
#[derive(Default)]
pub struct Search {
    pattern: String,
    regex: Option<RegexSearch>,
    current: Option<Match>,
}

/// Escapes `text` so the regex engine matches it literally.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if "\\.+*?()|[]{}^$#".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

impl Search {
    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    pub fn is_active(&self) -> bool {
        self.regex.is_some()
    }

    /// The focused match.
    pub fn current(&self) -> Option<&Match> {
        self.current.as_ref()
    }

    /// Sets the text to find. Matching ignores case unless the text has capitals.
    /// Returns `false` when the text can't be searched for, such as when it's too long.
    pub fn set_pattern(&mut self, pattern: &str) -> bool {
        self.pattern = pattern.to_string();
        self.current = None;
        if pattern.is_empty() {
            self.regex = None;
            return true;
        }
        self.regex = RegexSearch::new(&escape(pattern)).ok();
        self.regex.is_some()
    }

    pub fn clear(&mut self) {
        self.set_pattern("");
    }

    /// Moves to the next match in `direction`, wrapping around the buffer, and scrolls it into view.
    /// Without a focused match, the search starts at the bottom of the screen.
    pub fn advance<T: EventListener>(
        &mut self,
        term: &mut Term<T>,
        direction: SearchDirection,
    ) -> Option<Match> {
        let regex = self.regex.as_mut()?;
        let (origin, dir, side) = match (&self.current, direction) {
            (Some(m), SearchDirection::Older) => {
                (m.start().sub(term, Boundary::None, 1), Direction::Left, Side::Right)
            }
            (Some(m), SearchDirection::Newer) => {
                (m.end().add(term, Boundary::None, 1), Direction::Right, Side::Left)
            }
            (None, _) => {
                let bottom = Point::new(term.bottommost_line(), term.last_column());
                (bottom, Direction::Left, Side::Right)
            }
        };
        let found = term.search_next(regex, origin, dir, side, None);
        if let Some(m) = &found {
            term.scroll_to_point(*m.start());
        }
        self.current = found.clone();
        found
    }

    /// The matches on screen, for highlighting.
    pub fn visible_matches<T>(&mut self, term: &Term<T>) -> Vec<Match> {
        let Some(regex) = self.regex.as_mut() else {
            return Vec::new();
        };
        let offset = term.grid().display_offset() as i32;
        let start = Point::new(Line(-offset), Column(0));
        let end = Point::new(Line(term.screen_lines() as i32 - 1 - offset), term.last_column());
        RegexIter::new(start, end, Direction::Right, term, regex)
            .take(MAX_VISIBLE_MATCHES)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{feed, test_term};

    fn text_of<T>(term: &Term<T>, m: &Match) -> String {
        term.bounds_to_string(*m.start(), *m.end())
    }

    #[test]
    fn finds_newest_first_then_cycles() {
        let mut term = test_term(30, 4);
        feed(&mut term, b"apple one\r\nbanana\r\napple two\r\n");
        let mut search = Search::default();
        assert!(search.set_pattern("apple"));
        let first = search.advance(&mut term, SearchDirection::Older).expect("a match");
        assert_eq!(first.start().line, Line(2));
        assert_eq!(text_of(&term, &first), "apple");
        let older = search.advance(&mut term, SearchDirection::Older).expect("a match");
        assert_eq!(older.start().line, Line(0));
        let wrapped = search.advance(&mut term, SearchDirection::Older).expect("a match");
        assert_eq!(wrapped.start().line, Line(2));
        let newer = search.advance(&mut term, SearchDirection::Newer).expect("a match");
        assert_eq!(newer.start().line, Line(0));
        assert_eq!(search.current(), Some(&newer));
    }

    #[test]
    fn smart_case_and_literal_text() {
        let mut term = test_term(30, 3);
        feed(&mut term, b"Error: a.b (x) [y]\r\nerror again");
        let mut search = Search::default();
        assert!(search.set_pattern("error"));
        assert_eq!(search.visible_matches(&term).len(), 2);
        assert!(search.set_pattern("Error"));
        assert_eq!(search.visible_matches(&term).len(), 1);
        assert!(search.set_pattern("a.b (x) [y]"));
        assert_eq!(search.visible_matches(&term).len(), 1);
        assert!(search.set_pattern("a+b"));
        assert!(search.visible_matches(&term).is_empty());
        assert_eq!(search.advance(&mut term, SearchDirection::Older), None);
    }

    #[test]
    fn scrolls_to_matches_in_history() {
        let mut term = test_term(20, 3);
        feed(&mut term, b"needle\r\n");
        for i in 0..10 {
            feed(&mut term, format!("line {i}\r\n").as_bytes());
        }
        let mut search = Search::default();
        search.set_pattern("needle");
        assert!(search.visible_matches(&term).is_empty());
        let found = search.advance(&mut term, SearchDirection::Older).expect("a match in history");
        assert!(found.start().line.0 < 0);
        assert!(term.grid().display_offset() > 0);
        assert_eq!(search.visible_matches(&term).len(), 1);
    }

    #[test]
    fn empty_pattern_is_inactive() {
        let mut term = test_term(10, 2);
        let mut search = Search::default();
        assert!(search.set_pattern(""));
        assert!(!search.is_active());
        assert_eq!(search.advance(&mut term, SearchDirection::Newer), None);
        assert!(search.visible_matches(&term).is_empty());
        search.set_pattern("x");
        assert!(search.is_active());
        assert_eq!(search.pattern(), "x");
        search.clear();
        assert!(!search.is_active());
    }
}
