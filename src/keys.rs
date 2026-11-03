//! Reader keyboard shortcuts: actions, key chords, defaults, and matching.
//! Only bindings that differ from the defaults are stored in the settings.

use crate::i18n::tr;
use std::collections::BTreeMap;

use eframe::egui::{Key, Modifiers};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum KeyAction {
    NextPage,
    PrevPage,
    ScrollDown,
    ScrollUp,
    ChapterStart,
    ChapterEnd,
    Bookmark,
    Toolbar,
    Contents,
    Notebook,
    Settings,
    Highlighter,
    AutoAdvance,
    Search,
    Fullscreen,
    BiggerText,
    SmallerText,
    Back,
}

pub const ALL: [KeyAction; 18] = [
    KeyAction::NextPage,
    KeyAction::PrevPage,
    KeyAction::ScrollDown,
    KeyAction::ScrollUp,
    KeyAction::ChapterStart,
    KeyAction::ChapterEnd,
    KeyAction::Bookmark,
    KeyAction::Toolbar,
    KeyAction::Contents,
    KeyAction::Notebook,
    KeyAction::Settings,
    KeyAction::Highlighter,
    KeyAction::AutoAdvance,
    KeyAction::Search,
    KeyAction::Fullscreen,
    KeyAction::BiggerText,
    KeyAction::SmallerText,
    KeyAction::Back,
];

impl KeyAction {
    pub fn id(self) -> &'static str {
        match self {
            KeyAction::NextPage => "next",
            KeyAction::PrevPage => "prev",
            KeyAction::ScrollDown => "down",
            KeyAction::ScrollUp => "up",
            KeyAction::ChapterStart => "chapter-start",
            KeyAction::ChapterEnd => "chapter-end",
            KeyAction::Bookmark => "bookmark",
            KeyAction::Toolbar => "toolbar",
            KeyAction::Contents => "contents",
            KeyAction::Notebook => "notebook",
            KeyAction::Settings => "settings",
            KeyAction::Highlighter => "highlighter",
            KeyAction::AutoAdvance => "auto",
            KeyAction::Search => "search",
            KeyAction::Fullscreen => "fullscreen",
            KeyAction::BiggerText => "bigger",
            KeyAction::SmallerText => "smaller",
            KeyAction::Back => "back",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            KeyAction::NextPage => tr("Next page"),
            KeyAction::PrevPage => tr("Previous page"),
            KeyAction::ScrollDown => tr("Scroll down"),
            KeyAction::ScrollUp => tr("Scroll up"),
            KeyAction::ChapterStart => tr("Start of chapter"),
            KeyAction::ChapterEnd => tr("End of chapter"),
            KeyAction::Bookmark => tr("Bookmark this page"),
            KeyAction::Toolbar => tr("Show or hide toolbar"),
            KeyAction::Contents => tr("Contents sidebar"),
            KeyAction::Notebook => tr("Notebook"),
            KeyAction::Settings => tr("Reading settings"),
            KeyAction::Highlighter => tr("Highlighter mode"),
            KeyAction::AutoAdvance => tr("Auto-advance"),
            KeyAction::Search => tr("Search in the book"),
            KeyAction::Fullscreen => tr("Fullscreen"),
            KeyAction::BiggerText => tr("Bigger text"),
            KeyAction::SmallerText => tr("Smaller text"),
            KeyAction::Back => tr("Back to previous place"),
        }
    }

    pub fn defaults(self) -> &'static [&'static str] {
        match self {
            KeyAction::NextPage => &["ArrowRight", "PageDown", "Space", "J"],
            KeyAction::PrevPage => &["ArrowLeft", "PageUp", "K", "Shift+Space"],
            KeyAction::ScrollDown => &["ArrowDown"],
            KeyAction::ScrollUp => &["ArrowUp"],
            KeyAction::ChapterStart => &["Home"],
            KeyAction::ChapterEnd => &["End"],
            KeyAction::Bookmark => &["B", "D"],
            KeyAction::Toolbar => &["T"],
            KeyAction::Contents => &["C"],
            KeyAction::Notebook => &["N"],
            KeyAction::Settings => &["S"],
            KeyAction::Highlighter => &["H"],
            KeyAction::AutoAdvance => &["A"],
            KeyAction::Search => &["Ctrl+F", "Ctrl+K"],
            KeyAction::Fullscreen => &["F", "F11"],
            KeyAction::BiggerText => &["Ctrl+Plus", "Ctrl+Equals"],
            KeyAction::SmallerText => &["Ctrl+Minus"],
            KeyAction::Back => &["Backspace", "Alt+Backspace"],
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Chord {
    pub key: Key,
    /// Ctrl (Cmd on macOS)
    pub command: bool,
    pub shift: bool,
    pub alt: bool,
}

impl Chord {
    pub fn parse(s: &str) -> Option<Chord> {
        let mut c = Chord { key: Key::A, command: false, shift: false, alt: false };
        let mut key = None;
        for part in s.split('+') {
            match part.trim() {
                "Ctrl" | "Cmd" => c.command = true,
                "Shift" => c.shift = true,
                "Alt" => c.alt = true,
                "" => {}
                k => key = Key::from_name(k),
            }
        }
        c.key = key?;
        Some(c)
    }

    pub fn from_event(key: Key, m: Modifiers) -> Chord {
        Chord { key, command: m.command, shift: m.shift, alt: m.alt }
    }

    pub fn to_text(self) -> String {
        let mut s = String::new();
        if self.command {
            s.push_str("Ctrl+");
        }
        if self.alt {
            s.push_str("Alt+");
        }
        if self.shift {
            s.push_str("Shift+");
        }
        s.push_str(self.key.name());
        s
    }

    /// Human-readable, e.g. "Ctrl+K", "→", "Space".
    pub fn pretty(self) -> String {
        let k = match self.key {
            Key::ArrowRight => "→",
            Key::ArrowLeft => "←",
            Key::ArrowUp => "↑",
            Key::ArrowDown => "↓",
            Key::Plus => "+",
            Key::Minus => "−",
            Key::Equals => "=",
            k => k.name(),
        };
        let mut s = String::new();
        if self.command {
            s.push_str("Ctrl+");
        }
        if self.alt {
            s.push_str("Alt+");
        }
        if self.shift {
            s.push_str("Shift+");
        }
        s.push_str(k);
        s
    }

    /// Does this chord fire for `key` pressed with `m`? Shift is only required when the chord
    /// has it (so Ctrl+Plus still works on layouts where + needs Shift).
    fn matches(self, key: Key, m: Modifiers) -> bool {
        self.key == key && self.command == m.command && self.alt == m.alt && (!self.shift || m.shift)
    }
}

pub struct Keymap {
    bindings: Vec<(KeyAction, Vec<Chord>)>,
}

impl Keymap {
    pub fn from_settings(custom: &BTreeMap<String, Vec<String>>) -> Keymap {
        let bindings = ALL
            .iter()
            .map(|&a| {
                let chords = match custom.get(a.id()) {
                    Some(v) => v.iter().filter_map(|s| Chord::parse(s)).collect(),
                    None => a.defaults().iter().filter_map(|s| Chord::parse(s)).collect(),
                };
                (a, chords)
            })
            .collect();
        Keymap { bindings }
    }

    pub fn chords(&self, a: KeyAction) -> &[Chord] {
        self.bindings.iter().find(|(x, _)| *x == a).map(|(_, c)| c.as_slice()).unwrap_or(&[])
    }

    /// The action bound to this key press, preferring chords that name Shift.
    pub fn action_for(&self, key: Key, m: Modifiers) -> Option<KeyAction> {
        let mut plain = None;
        for (a, chords) in &self.bindings {
            for c in chords {
                if c.matches(key, m) {
                    if c.shift {
                        return Some(*a);
                    }
                    plain.get_or_insert(*a);
                }
            }
        }
        plain
    }

    /// Another action that already uses exactly this chord.
    pub fn conflict(&self, chord: Chord, except: KeyAction) -> Option<KeyAction> {
        self.bindings.iter().find(|(a, cs)| *a != except && cs.contains(&chord)).map(|(a, _)| *a)
    }
}

/// Store `chords` for `a`, dropping the override when it equals the defaults.
pub fn set(custom: &mut BTreeMap<String, Vec<String>>, a: KeyAction, chords: Vec<Chord>) {
    let texts: Vec<String> = chords.iter().map(|c| c.to_text()).collect();
    let def: Vec<String> = a.defaults().iter().filter_map(|s| Chord::parse(s)).map(|c| c.to_text()).collect();
    if texts == def {
        custom.remove(a.id());
    } else {
        custom.insert(a.id().to_string(), texts);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_parse_and_match_readme() {
        let km = Keymap::from_settings(&BTreeMap::new());
        for a in ALL {
            assert_eq!(km.chords(a).len(), a.defaults().len(), "{a:?}");
        }
        let none = Modifiers::NONE;
        let ctrl = Modifiers::COMMAND;
        assert_eq!(km.action_for(Key::ArrowRight, none), Some(KeyAction::NextPage));
        assert_eq!(km.action_for(Key::Space, none), Some(KeyAction::NextPage));
        assert_eq!(km.action_for(Key::Space, Modifiers::SHIFT), Some(KeyAction::PrevPage));
        assert_eq!(km.action_for(Key::K, none), Some(KeyAction::PrevPage));
        assert_eq!(km.action_for(Key::K, ctrl), Some(KeyAction::Search));
        assert_eq!(km.action_for(Key::F, ctrl), Some(KeyAction::Search));
        assert_eq!(km.action_for(Key::F, none), Some(KeyAction::Fullscreen));
        // an unbound Ctrl chord must not fall through to the plain key
        assert_eq!(km.action_for(Key::B, ctrl), None);
        assert_eq!(km.action_for(Key::Plus, ctrl | Modifiers::SHIFT), Some(KeyAction::BiggerText));
        assert_eq!(km.action_for(Key::Backspace, none), Some(KeyAction::Back));
    }

    #[test]
    fn no_default_conflicts() {
        let km = Keymap::from_settings(&BTreeMap::new());
        for a in ALL {
            for c in km.chords(a) {
                assert_eq!(km.conflict(*c, a), None, "{a:?} {}", c.to_text());
            }
        }
    }

    #[test]
    fn custom_roundtrip() {
        let mut m = BTreeMap::new();
        let c = Chord::parse("Ctrl+Shift+G").unwrap();
        assert_eq!(c.to_text(), "Ctrl+Shift+G");
        set(&mut m, KeyAction::Bookmark, vec![c]);
        let km = Keymap::from_settings(&m);
        assert_eq!(km.action_for(Key::B, Modifiers::NONE), None);
        assert_eq!(km.action_for(Key::G, Modifiers::COMMAND | Modifiers::SHIFT), Some(KeyAction::Bookmark));
        assert_eq!(km.conflict(c, KeyAction::Toolbar), Some(KeyAction::Bookmark));
        // restoring the defaults removes the override
        set(&mut m, KeyAction::Bookmark, KeyAction::Bookmark.defaults().iter().filter_map(|s| Chord::parse(s)).collect());
        assert!(m.is_empty());
    }
}
