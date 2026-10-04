//! Windows virtual-key codes for the keys a `KeyChord` can name.
//!
//! Codes are the stable Win32 `VK_*` protocol constants — taking a bare
//! `u16` keeps the table decidable off Windows, the same trick
//! `control_type_name` uses for ControlType ids. Unknown keys return
//! `None`: the caller reports `Unsupported`, never guesses a key.

/// Canonical key name → virtual-key code.
pub fn vk_of(name: &str) -> Option<u16> {
    // Single chars: VK_A..VK_Z and VK_0..VK_9 are the ASCII code point.
    if name.len() == 1 {
        let c = name.chars().next()?.to_ascii_uppercase();
        if c.is_ascii_uppercase() || c.is_ascii_digit() {
            return Some(c as u16);
        }
    }
    // f1..f24 are VK_F1..VK_F24 (0x70..0x87).
    if let Some(n) = name
        .strip_prefix('f')
        .and_then(|s| s.parse::<u16>().ok())
        .filter(|n| (1..=24).contains(n))
    {
        return Some(0x70 + n - 1);
    }
    Some(match name {
        "backspace" => 0x08,                         // VK_BACK
        "tab" => 0x09,                               // VK_TAB
        "clear" => 0x0C,                             // VK_CLEAR
        "return" | "enter" => 0x0D,                  // VK_RETURN
        "pause" => 0x13,                             // VK_PAUSE
        "capslock" | "caps_lock" => 0x14,            // VK_CAPITAL
        "escape" | "esc" => 0x1B,                    // VK_ESCAPE
        "space" => 0x20,                             // VK_SPACE
        "page_up" | "pageup" => 0x21,                // VK_PRIOR
        "page_down" | "pagedown" => 0x22,            // VK_NEXT
        "end" => 0x23,                               // VK_END
        "home" => 0x24,                              // VK_HOME
        "left" => 0x25,                              // VK_LEFT
        "up" => 0x26,                                // VK_UP
        "right" => 0x27,                             // VK_RIGHT
        "down" => 0x28,                              // VK_DOWN
        "printscreen" | "print_screen" => 0x2C,      // VK_SNAPSHOT
        "insert" => 0x2D,                            // VK_INSERT
        "delete" | "del" | "forward_delete" => 0x2E, // VK_DELETE
        "win" | "windows" | "super" => 0x5B,         // VK_LWIN
        "numlock" | "num_lock" => 0x90,              // VK_NUMLOCK
        "scrolllock" | "scroll_lock" => 0x91,        // VK_SCROLL
        ";" | "semicolon" => 0xBA,                   // VK_OEM_1
        "=" | "equal" | "plus" => 0xBB,              // VK_OEM_PLUS
        "," | "comma" => 0xBC,                       // VK_OEM_COMMA
        "-" | "minus" => 0xBD,                       // VK_OEM_MINUS
        "." | "period" => 0xBE,                      // VK_OEM_PERIOD
        "/" | "slash" => 0xBF,                       // VK_OEM_2
        "`" | "backtick" | "backquote" => 0xC0,      // VK_OEM_3
        "[" | "bracketleft" => 0xDB,                 // VK_OEM_4
        "\\" | "backslash" => 0xDC,                  // VK_OEM_5
        "]" | "bracketright" => 0xDD,                // VK_OEM_6
        "'" | "quote" => 0xDE,                       // VK_OEM_7
        _ => return None,
    })
}

/// Modifier name → virtual-key code held for the chord. `cmd` is the
/// Windows key on this platform — the canonical chord name parses
/// through, the platform key is what actually goes down.
pub fn modifier_vk(name: &str) -> Option<u16> {
    Some(match name {
        "shift" => 0x10,                    // VK_SHIFT
        "ctrl" | "control" => 0x11,         // VK_CONTROL
        "alt" | "option" | "opt" => 0x12,   // VK_MENU
        "cmd" | "command" | "meta" => 0x5B, // VK_LWIN
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_and_digits_use_ascii_codes() {
        assert_eq!(vk_of("a"), Some(0x41));
        assert_eq!(vk_of("z"), Some(0x5A));
        assert_eq!(vk_of("0"), Some(0x30));
        assert_eq!(vk_of("9"), Some(0x39));
        // Single chars that have no VK alias stay unmapped.
        assert_eq!(vk_of("~"), None);
    }

    #[test]
    fn function_keys_run_f1_through_f24() {
        assert_eq!(vk_of("f1"), Some(0x70));
        assert_eq!(vk_of("f12"), Some(0x7B));
        assert_eq!(vk_of("f24"), Some(0x87));
        assert_eq!(vk_of("f0"), None);
        assert_eq!(vk_of("f25"), None);
        assert_eq!(vk_of("fx"), None);
    }

    #[test]
    fn named_keys_cover_the_chord_vocabulary() {
        for (name, vk) in [
            ("return", 0x0D),
            ("enter", 0x0D),
            ("tab", 0x09),
            ("escape", 0x1B),
            ("esc", 0x1B),
            ("space", 0x20),
            ("backspace", 0x08),
            ("delete", 0x2E),
            ("insert", 0x2D),
            ("home", 0x24),
            ("end", 0x23),
            ("pageup", 0x21),
            ("pagedown", 0x22),
            ("left", 0x25),
            ("right", 0x27),
            ("up", 0x26),
            ("down", 0x28),
            ("semicolon", 0xBA),
            ("equal", 0xBB),
            ("comma", 0xBC),
            ("minus", 0xBD),
            ("period", 0xBE),
            ("slash", 0xBF),
            ("backtick", 0xC0),
            ("[", 0xDB),
            ("backslash", 0xDC),
            ("]", 0xDD),
            ("quote", 0xDE),
        ] {
            assert_eq!(vk_of(name), Some(vk), "{name}");
        }
        assert_eq!(vk_of("hyper"), None);
        assert_eq!(vk_of(""), None);
    }

    #[test]
    fn modifiers_map_and_cmd_is_the_windows_key() {
        assert_eq!(modifier_vk("shift"), Some(0x10));
        assert_eq!(modifier_vk("ctrl"), Some(0x11));
        assert_eq!(modifier_vk("control"), Some(0x11));
        assert_eq!(modifier_vk("alt"), Some(0x12));
        assert_eq!(modifier_vk("option"), Some(0x12));
        // `cmd` parses from the canonical chord vocabulary and lands on
        // the platform's window key — never on a nonexistent key.
        assert_eq!(modifier_vk("cmd"), Some(0x5B));
        assert_eq!(modifier_vk("meta"), Some(0x5B));
        assert_eq!(modifier_vk("hyper"), None);
    }
}
