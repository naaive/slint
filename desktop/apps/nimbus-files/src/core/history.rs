// SPDX-License-Identifier: MIT

//! Back and forward navigation.

use std::path::PathBuf;

/// A place the window can show.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Location {
    Dir(PathBuf),
    Trash,
}

impl Location {
    pub fn dir(&self) -> Option<&std::path::Path> {
        match self {
            Location::Dir(path) => Some(path),
            Location::Trash => None,
        }
    }
}

/// The visited locations with a cursor; going somewhere new drops the forward entries.
#[derive(Clone, Debug)]
pub struct History {
    entries: Vec<Location>,
    index: usize,
}

/// Visits beyond this many are forgotten, oldest first.
const MAX_ENTRIES: usize = 200;

impl History {
    pub fn new(start: Location) -> Self {
        Self { entries: vec![start], index: 0 }
    }

    pub fn current(&self) -> &Location {
        &self.entries[self.index]
    }

    /// Records a visit; visiting the current location again is a no-op.
    pub fn visit(&mut self, location: Location) {
        if *self.current() == location {
            return;
        }
        self.entries.truncate(self.index + 1);
        self.entries.push(location);
        if self.entries.len() > MAX_ENTRIES {
            self.entries.remove(0);
        }
        self.index = self.entries.len() - 1;
    }

    /// Replaces the current location without a new history entry, such as after a rename of the current folder.
    pub fn replace_current(&mut self, location: Location) {
        self.entries[self.index] = location;
    }

    pub fn can_go_back(&self) -> bool {
        self.index > 0
    }

    pub fn can_go_forward(&self) -> bool {
        self.index + 1 < self.entries.len()
    }

    pub fn back(&mut self) -> Option<&Location> {
        self.can_go_back().then(|| {
            self.index -= 1;
            &self.entries[self.index]
        })
    }

    pub fn forward(&mut self) -> Option<&Location> {
        self.can_go_forward().then(|| {
            self.index += 1;
            &self.entries[self.index]
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(p: &str) -> Location {
        Location::Dir(PathBuf::from(p))
    }

    #[test]
    fn back_and_forward() {
        let mut history = History::new(dir("/a"));
        assert!(!history.can_go_back() && !history.can_go_forward());
        assert_eq!(history.back(), None);
        history.visit(dir("/b"));
        history.visit(dir("/b"));
        history.visit(Location::Trash);
        assert_eq!(history.back(), Some(&dir("/b")));
        assert_eq!(history.back(), Some(&dir("/a")));
        assert_eq!(history.back(), None);
        assert_eq!(history.forward(), Some(&dir("/b")));
        history.visit(dir("/c"));
        assert!(!history.can_go_forward());
        assert_eq!(history.back(), Some(&dir("/b")));
        history.replace_current(dir("/b2"));
        assert_eq!(history.current(), &dir("/b2"));
        assert_eq!(history.current().dir(), Some(std::path::Path::new("/b2")));
        assert_eq!(Location::Trash.dir(), None);
    }

    #[test]
    fn bounded() {
        let mut history = History::new(dir("/0"));
        for i in 1..=MAX_ENTRIES + 10 {
            history.visit(dir(&format!("/{i}")));
        }
        let mut steps = 0;
        while history.back().is_some() {
            steps += 1;
        }
        assert_eq!(steps, MAX_ENTRIES - 1);
    }
}
