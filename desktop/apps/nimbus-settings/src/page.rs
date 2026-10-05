// SPDX-License-Identifier: MIT

//! The pages of the settings app, in sidebar order.

use std::fmt;
use std::str::FromStr;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Page {
    #[default]
    Appearance,
    Panel,
    Workspaces,
    Input,
    Shortcuts,
    Power,
    Displays,
    Notifications,
    About,
}

impl Page {
    pub const ALL: [Page; 9] = [
        Page::Appearance,
        Page::Panel,
        Page::Workspaces,
        Page::Input,
        Page::Shortcuts,
        Page::Power,
        Page::Displays,
        Page::Notifications,
        Page::About,
    ];

    /// The identifier used by `--page`.
    pub fn id(self) -> &'static str {
        match self {
            Page::Appearance => "appearance",
            Page::Panel => "panel",
            Page::Workspaces => "workspaces",
            Page::Input => "input",
            Page::Shortcuts => "shortcuts",
            Page::Power => "power",
            Page::Displays => "displays",
            Page::Notifications => "notifications",
            Page::About => "about",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Page::Appearance => "Appearance",
            Page::Panel => "Panel & Dock",
            Page::Workspaces => "Workspaces & Windows",
            Page::Input => "Keyboard & Mouse",
            Page::Shortcuts => "Shortcuts",
            Page::Power => "Power",
            Page::Displays => "Displays",
            Page::Notifications => "Notifications",
            Page::About => "About",
        }
    }

    /// The position in the sidebar, which the UI uses as the page number.
    pub fn index(self) -> usize {
        Page::ALL.iter().position(|p| *p == self).unwrap_or(0)
    }

    pub fn from_index(index: i32) -> Option<Page> {
        usize::try_from(index).ok().and_then(|i| Page::ALL.get(i).copied())
    }
}

impl fmt::Display for Page {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "unknown page '{0}'; expected one of: appearance, panel, workspaces, input, shortcuts, power, displays, notifications, about"
)]
pub struct UnknownPage(pub String);

impl FromStr for Page {
    type Err = UnknownPage;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let wanted = s.trim().to_ascii_lowercase();
        Page::ALL.into_iter().find(|p| p.id() == wanted).ok_or_else(|| UnknownPage(s.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_and_indices_match_order() {
        for (i, page) in Page::ALL.into_iter().enumerate() {
            assert_eq!(page.id().parse::<Page>(), Ok(page));
            assert_eq!(page.index(), i);
            assert_eq!(Page::from_index(i as i32), Some(page));
        }
        assert_eq!(" About ".parse::<Page>(), Ok(Page::About));
        assert!("sound".parse::<Page>().is_err());
        assert_eq!(Page::from_index(-1), None);
        assert_eq!(Page::from_index(9), None);
    }
}
