// Push-to-talk key tracking.
//
// The low-level keyboard hook (installed in main.rs) feeds every key event
// into `PttTracker`, which decides when recording starts and stops. The
// tracker itself is pure state — no Win32 — so it's unit-testable.
//
// Mirrors the macOS HotkeyMonitor semantics:
// * single key: hold to record, release to transcribe;
// * combo "either": either of two keys works interchangeably — recording
//   runs while at least one is held;
// * combo "both": recording runs only while both are held together.

/// Virtual-key codes for the hotkeys we offer. Names are what appears in
/// config.toml.
pub fn parse_hotkey(name: &str) -> Option<u32> {
    let vk = match name.trim().to_lowercase().as_str() {
        "left_ctrl" | "lctrl" => 0xA2,
        "right_ctrl" | "rctrl" => 0xA3,
        "left_alt" | "lalt" => 0xA4,
        // Right Alt doubles as AltGr on some layouts — supported, but not
        // the default for that reason.
        "right_alt" | "ralt" | "altgr" => 0xA5,
        "left_shift" | "lshift" => 0xA0,
        "right_shift" | "rshift" => 0xA1,
        "left_win" | "lwin" => 0x5B,
        "right_win" | "rwin" => 0x5C,
        "caps_lock" | "capslock" => 0x14,
        "scroll_lock" | "scrolllock" => 0x91,
        "pause" => 0x13,
        "" | "none" => return None,
        f if f.starts_with('f') => {
            // f1..f24 → VK_F1 (0x70) .. VK_F24 (0x87)
            let n: u32 = f[1..].parse().ok()?;
            if (1..=24).contains(&n) {
                0x70 + n - 1
            } else {
                return None;
            }
        }
        _ => return None,
    };
    Some(vk)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ComboMode {
    /// Either key alone triggers recording.
    Either,
    /// Both keys must be held together.
    Both,
}

impl ComboMode {
    pub fn parse(s: &str) -> ComboMode {
        match s.trim().to_lowercase().as_str() {
            "both" => ComboMode::Both,
            _ => ComboMode::Either,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PttEvent {
    Start,
    Stop,
}

pub struct PttTracker {
    primary: u32,
    secondary: Option<u32>,
    mode: ComboMode,
    primary_down: bool,
    secondary_down: bool,
    active: bool,
}

impl PttTracker {
    pub fn new(primary: u32, secondary: Option<u32>, mode: ComboMode) -> Self {
        Self {
            primary,
            secondary,
            mode,
            primary_down: false,
            secondary_down: false,
            active: false,
        }
    }

    /// Feed one key transition; returns Start/Stop when the push-to-talk
    /// state flips.
    pub fn on_key(&mut self, vk: u32, down: bool) -> Option<PttEvent> {
        if vk == self.primary {
            self.primary_down = down;
        } else if Some(vk) == self.secondary {
            self.secondary_down = down;
        } else {
            return None;
        }

        let should_be_active = match (self.secondary, self.mode) {
            (None, _) => self.primary_down,
            (Some(_), ComboMode::Either) => self.primary_down || self.secondary_down,
            (Some(_), ComboMode::Both) => self.primary_down && self.secondary_down,
        };

        if should_be_active && !self.active {
            self.active = true;
            Some(PttEvent::Start)
        } else if !should_be_active && self.active {
            self.active = false;
            Some(PttEvent::Stop)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_hotkeys() {
        assert_eq!(parse_hotkey("right_ctrl"), Some(0xA3));
        assert_eq!(parse_hotkey("LEFT_ALT"), Some(0xA4));
        assert_eq!(parse_hotkey("f13"), Some(0x7C));
        assert_eq!(parse_hotkey("f1"), Some(0x70));
        assert_eq!(parse_hotkey("caps_lock"), Some(0x14));
    }

    #[test]
    fn rejects_unknown_hotkeys() {
        assert_eq!(parse_hotkey("hyper"), None);
        assert_eq!(parse_hotkey("f0"), None);
        assert_eq!(parse_hotkey("f25"), None);
        assert_eq!(parse_hotkey(""), None);
        assert_eq!(parse_hotkey("none"), None);
    }

    #[test]
    fn single_key_hold_release() {
        let mut t = PttTracker::new(0xA3, None, ComboMode::Either);
        assert_eq!(t.on_key(0xA3, true), Some(PttEvent::Start));
        // Auto-repeat down events must not re-trigger.
        assert_eq!(t.on_key(0xA3, true), None);
        assert_eq!(t.on_key(0xA3, false), Some(PttEvent::Stop));
        assert_eq!(t.on_key(0xA3, false), None);
    }

    #[test]
    fn other_keys_are_ignored() {
        let mut t = PttTracker::new(0xA3, None, ComboMode::Either);
        assert_eq!(t.on_key(0x41, true), None); // 'A'
        assert_eq!(t.on_key(0xA3, true), Some(PttEvent::Start));
        assert_eq!(t.on_key(0x41, false), None);
        assert_eq!(t.on_key(0xA3, false), Some(PttEvent::Stop));
    }

    #[test]
    fn either_mode_keys_are_interchangeable() {
        let mut t = PttTracker::new(0xA3, Some(0xA1), ComboMode::Either);
        assert_eq!(t.on_key(0xA1, true), Some(PttEvent::Start));
        // Pressing the second key while active changes nothing.
        assert_eq!(t.on_key(0xA3, true), None);
        // Releasing one while the other is held keeps recording.
        assert_eq!(t.on_key(0xA1, false), None);
        assert_eq!(t.on_key(0xA3, false), Some(PttEvent::Stop));
    }

    #[test]
    fn both_mode_requires_chord() {
        let mut t = PttTracker::new(0xA3, Some(0xA1), ComboMode::Both);
        assert_eq!(t.on_key(0xA3, true), None);
        assert_eq!(t.on_key(0xA1, true), Some(PttEvent::Start));
        assert_eq!(t.on_key(0xA1, false), Some(PttEvent::Stop));
        assert_eq!(t.on_key(0xA3, false), None);
    }

    #[test]
    fn combo_mode_parse() {
        assert_eq!(ComboMode::parse("both"), ComboMode::Both);
        assert_eq!(ComboMode::parse("either"), ComboMode::Either);
        assert_eq!(ComboMode::parse("bogus"), ComboMode::Either);
    }
}
