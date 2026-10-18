pub fn key_to_keysym(name: &str) -> Option<u32> {
    if let [byte] = name.as_bytes() {
        if byte.is_ascii_lowercase() || byte.is_ascii_digit() {
            return Some(u32::from(*byte));
        }
    }
    let keysym = match name {
        "return" => 0xff0d,
        "tab" => 0xff09,
        "escape" => 0xff1b,
        "space" => 0x0020,
        "backspace" => 0xff08,
        "delete" => 0xffff,
        "left" => 0xff51,
        "up" => 0xff52,
        "right" => 0xff53,
        "down" => 0xff54,
        "home" => 0xff50,
        "end" => 0xff57,
        "pageup" => 0xff55,
        "pagedown" => 0xff56,
        "f1" => 0xffbe,
        "f2" => 0xffbf,
        "f3" => 0xffc0,
        "f4" => 0xffc1,
        "f5" => 0xffc2,
        "f6" => 0xffc3,
        "f7" => 0xffc4,
        "f8" => 0xffc5,
        "f9" => 0xffc6,
        "f10" => 0xffc7,
        "f11" => 0xffc8,
        "f12" => 0xffc9,
        _ => return None,
    };
    Some(keysym)
}

pub fn char_to_keysym(character: char) -> u32 {
    let codepoint = character as u32;
    if codepoint <= 0xff {
        codepoint
    } else {
        0x0100_0000 + codepoint
    }
}

fn wheel_button(delta: f64, positive: u8, negative: u8) -> Option<(u8, u32)> {
    let notches = (delta / 40.0).round();
    if !notches.is_finite() || notches == 0.0 {
        None
    } else {
        Some((
            if notches > 0.0 { positive } else { negative },
            notches.abs() as u32,
        ))
    }
}

pub fn scroll_buttons(dx: f64, dy: f64) -> Vec<(u8, u32)> {
    let mut buttons = Vec::with_capacity(2);
    if let Some(vertical) = wheel_button(dy, 5, 4) {
        buttons.push(vertical);
    }
    if let Some(horizontal) = wheel_button(dx, 7, 6) {
        buttons.push(horizontal);
    }
    buttons
}

pub fn clamp_scroll_value(current: f64, delta: f64, min: f64, max: f64) -> f64 {
    (current + delta).clamp(min, max)
}

#[cfg(target_os = "linux")]
mod x11 {
    use super::{char_to_keysym, key_to_keysym, scroll_buttons};
    use dexter_core::{KeyChord, MouseButton};
    use dexter_driver::DriverError;
    use std::convert::TryFrom;
    use x11rb::{
        connection::{Connection, RequestConnection},
        protocol::{
            xproto::{
                ConnectionExt as _, Window, BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT,
                KEY_PRESS_EVENT, KEY_RELEASE_EVENT, MOTION_NOTIFY_EVENT,
            },
            xtest::{self, ConnectionExt as _},
        },
        rust_connection::RustConnection,
    };

    pub(crate) enum InputOutcome<T> {
        Complete(T),
        Unsupported(String),
    }

    struct KeyMap {
        min_keycode: u8,
        keysyms_per_keycode: usize,
        keysyms: Vec<u32>,
    }

    impl KeyMap {
        fn keycode_for(&self, keysym: u32) -> Option<u8> {
            self.keysyms
                .chunks(self.keysyms_per_keycode)
                .position(|symbols| symbols.contains(&keysym))
                .and_then(|offset| u8::try_from(offset).ok())
                .and_then(|offset| self.min_keycode.checked_add(offset))
        }

        fn character_keycode(&self, keysym: u32) -> Option<(u8, bool)> {
            self.keysyms
                .chunks(self.keysyms_per_keycode)
                .enumerate()
                .find_map(|(offset, symbols)| {
                    let shifted = if symbols.first() == Some(&keysym) {
                        false
                    } else if symbols.get(1) == Some(&keysym) {
                        true
                    } else {
                        return None;
                    };
                    let keycode = u8::try_from(offset)
                        .ok()
                        .and_then(|offset| self.min_keycode.checked_add(offset))?;
                    Some((keycode, shifted))
                })
        }
    }

    fn connect() -> Result<InputOutcome<(RustConnection, usize)>, DriverError> {
        if crate::capture::wayland_active() {
            return Ok(InputOutcome::Unsupported(
                "XTest input is unavailable in Wayland sessions".into(),
            ));
        }
        let (connection, screen_num) = x11rb::connect(None).map_err(|error| {
            DriverError::Unsupported(format!("XTest could not connect to the X display: {error}"))
        })?;
        let extension = connection
            .extension_information(xtest::X11_EXTENSION_NAME)
            .map_err(|error| {
                DriverError::Platform(format!("X11 extension query failed: {error}"))
            })?;
        if extension.is_none() {
            return Ok(InputOutcome::Unsupported(
                "XTEST extension is unavailable".into(),
            ));
        }
        Ok(InputOutcome::Complete((connection, screen_num)))
    }

    fn root_window(connection: &RustConnection, screen_num: usize) -> Result<Window, DriverError> {
        connection
            .setup()
            .roots
            .get(screen_num)
            .map(|screen| screen.root)
            .ok_or_else(|| DriverError::Platform("X11 screen is unavailable".into()))
    }

    fn keyboard_map(connection: &RustConnection) -> Result<KeyMap, DriverError> {
        let setup = connection.setup();
        let count = setup
            .max_keycode
            .checked_sub(setup.min_keycode)
            .and_then(|count| count.checked_add(1))
            .ok_or_else(|| DriverError::Platform("invalid X11 keycode range".into()))?;
        let reply = connection
            .get_keyboard_mapping(setup.min_keycode, count)
            .map_err(|error| DriverError::Platform(format!("X11 keymap request failed: {error}")))?
            .reply()
            .map_err(|error| DriverError::Platform(format!("X11 keymap reply failed: {error}")))?;
        let keysyms_per_keycode = usize::from(reply.keysyms_per_keycode);
        if keysyms_per_keycode == 0 {
            return Err(DriverError::Platform(
                "X11 returned an empty keyboard mapping".into(),
            ));
        }
        Ok(KeyMap {
            min_keycode: setup.min_keycode,
            keysyms_per_keycode,
            keysyms: reply.keysyms,
        })
    }

    fn coordinate(value: f64, axis: &str) -> Result<i16, String> {
        let rounded = value.round();
        if !rounded.is_finite() || rounded < f64::from(i16::MIN) || rounded > f64::from(i16::MAX) {
            return Err(format!("XTest {axis} coordinate is outside the X11 range"));
        }
        Ok(rounded as i16)
    }

    fn fake_input(
        connection: &RustConnection,
        event_type: u8,
        detail: u8,
        root: Window,
        root_x: i16,
        root_y: i16,
    ) -> Result<(), DriverError> {
        connection
            .xtest_fake_input(event_type, detail, 0, root, root_x, root_y, 0)
            .map_err(|error| DriverError::Platform(format!("XTest FakeInput failed: {error}")))?
            .check()
            .map_err(|error| DriverError::Platform(format!("XTest FakeInput failed: {error}")))
    }

    fn button_number(button: MouseButton) -> u8 {
        match button {
            MouseButton::Left => 1,
            MouseButton::Right => 3,
            MouseButton::Middle => 2,
        }
    }

    pub(crate) fn click(
        x: f64,
        y: f64,
        button: MouseButton,
    ) -> Result<InputOutcome<()>, DriverError> {
        let (connection, screen_num) = match connect()? {
            InputOutcome::Complete(connection) => connection,
            InputOutcome::Unsupported(reason) => return Ok(InputOutcome::Unsupported(reason)),
        };
        let root = root_window(&connection, screen_num)?;
        let x = match coordinate(x, "x") {
            Ok(value) => value,
            Err(reason) => return Ok(InputOutcome::Unsupported(reason)),
        };
        let y = match coordinate(y, "y") {
            Ok(value) => value,
            Err(reason) => return Ok(InputOutcome::Unsupported(reason)),
        };
        let button = button_number(button);
        fake_input(&connection, MOTION_NOTIFY_EVENT, 0, root, x, y)?;
        fake_input(&connection, BUTTON_PRESS_EVENT, button, root, x, y)?;
        fake_input(&connection, BUTTON_RELEASE_EVENT, button, root, x, y)?;
        Ok(InputOutcome::Complete(()))
    }

    fn modifier_keysym(name: &str) -> Option<u32> {
        match name {
            "cmd" => Some(0xffeb),
            "ctrl" => Some(0xffe3),
            "alt" => Some(0xffe9),
            "shift" => Some(0xffe1),
            _ => None,
        }
    }

    pub(crate) fn key_chord(chord: &KeyChord) -> Result<InputOutcome<()>, DriverError> {
        let (connection, screen_num) = match connect()? {
            InputOutcome::Complete(connection) => connection,
            InputOutcome::Unsupported(reason) => return Ok(InputOutcome::Unsupported(reason)),
        };
        let keymap = keyboard_map(&connection)?;
        let Some(keysym) = key_to_keysym(&chord.key) else {
            return Ok(InputOutcome::Unsupported(format!(
                "unknown key name {:?}",
                chord.key
            )));
        };
        let Some(keycode) = keymap.keycode_for(keysym) else {
            return Ok(InputOutcome::Unsupported(format!(
                "no X11 keycode for key {:?}",
                chord.key
            )));
        };
        let mut modifiers = Vec::with_capacity(chord.modifiers.len());
        for name in &chord.modifiers {
            let Some(keysym) = modifier_keysym(name) else {
                return Ok(InputOutcome::Unsupported(format!(
                    "unknown key modifier {:?}",
                    name
                )));
            };
            let Some(keycode) = keymap.keycode_for(keysym) else {
                return Ok(InputOutcome::Unsupported(format!(
                    "no X11 keycode for modifier {:?}",
                    name
                )));
            };
            modifiers.push(keycode);
        }
        let root = root_window(&connection, screen_num)?;
        for modifier in &modifiers {
            fake_input(&connection, KEY_PRESS_EVENT, *modifier, root, 0, 0)?;
        }
        fake_input(&connection, KEY_PRESS_EVENT, keycode, root, 0, 0)?;
        fake_input(&connection, KEY_RELEASE_EVENT, keycode, root, 0, 0)?;
        for modifier in modifiers.iter().rev() {
            fake_input(&connection, KEY_RELEASE_EVENT, *modifier, root, 0, 0)?;
        }
        Ok(InputOutcome::Complete(()))
    }

    pub(crate) fn type_text(text: &str) -> Result<InputOutcome<usize>, DriverError> {
        let (connection, screen_num) = match connect()? {
            InputOutcome::Complete(connection) => connection,
            InputOutcome::Unsupported(reason) => return Ok(InputOutcome::Unsupported(reason)),
        };
        let keymap = keyboard_map(&connection)?;
        let shift_keycode = keymap.keycode_for(0xffe1);
        let mut plan = Vec::with_capacity(text.chars().count());
        for character in text.chars() {
            let keysym = char_to_keysym(character);
            let Some((keycode, shifted)) = keymap.character_keycode(keysym) else {
                return Ok(InputOutcome::Unsupported(format!(
                    "XTest cannot map character {character:?}"
                )));
            };
            let shift = if shifted {
                let Some(shift_keycode) = shift_keycode else {
                    return Ok(InputOutcome::Unsupported(format!(
                        "XTest cannot map character {character:?}"
                    )));
                };
                Some(shift_keycode)
            } else {
                None
            };
            plan.push((keycode, shift));
        }
        let root = root_window(&connection, screen_num)?;
        for (keycode, shift) in plan {
            if let Some(shift) = shift {
                fake_input(&connection, KEY_PRESS_EVENT, shift, root, 0, 0)?;
            }
            fake_input(&connection, KEY_PRESS_EVENT, keycode, root, 0, 0)?;
            fake_input(&connection, KEY_RELEASE_EVENT, keycode, root, 0, 0)?;
            if let Some(shift) = shift {
                fake_input(&connection, KEY_RELEASE_EVENT, shift, root, 0, 0)?;
            }
        }
        Ok(InputOutcome::Complete(text.chars().count()))
    }

    pub(crate) fn scroll(dx: f64, dy: f64) -> Result<InputOutcome<()>, DriverError> {
        let buttons = scroll_buttons(dx, dy);
        if buttons.is_empty() {
            return Ok(InputOutcome::Complete(()));
        }
        let (connection, screen_num) = match connect()? {
            InputOutcome::Complete(connection) => connection,
            InputOutcome::Unsupported(reason) => return Ok(InputOutcome::Unsupported(reason)),
        };
        let root = root_window(&connection, screen_num)?;
        for (button, notches) in buttons {
            for _ in 0..notches {
                fake_input(&connection, BUTTON_PRESS_EVENT, button, root, 0, 0)?;
                fake_input(&connection, BUTTON_RELEASE_EVENT, button, root, 0, 0)?;
            }
        }
        Ok(InputOutcome::Complete(()))
    }
}

#[cfg(target_os = "linux")]
pub(crate) use x11::{click, key_chord, scroll, type_text, InputOutcome};

#[cfg(test)]
mod tests {
    use super::{char_to_keysym, clamp_scroll_value, key_to_keysym, scroll_buttons};

    #[test]
    fn key_names_resolve_to_keysyms_and_unknown_names_do_not() {
        assert_eq!(key_to_keysym("a"), Some(u32::from(b'a')));
        assert_eq!(key_to_keysym("9"), Some(u32::from(b'9')));
        assert_eq!(key_to_keysym("return"), Some(0xff0d));
        assert_eq!(key_to_keysym("pageup"), Some(0xff55));
        assert_eq!(key_to_keysym("f12"), Some(0xffc9));
        assert_eq!(key_to_keysym("unknown"), None);
    }

    #[test]
    fn characters_use_latin_one_and_unicode_keysyms() {
        assert_eq!(char_to_keysym('é'), 0x00e9);
        assert_eq!(char_to_keysym('😀'), 0x0101_f600);
    }

    #[test]
    fn wheel_delta_rounds_to_axis_buttons_and_notches() {
        assert_eq!(scroll_buttons(85.0, -45.0), vec![(4, 1), (7, 2)]);
        assert_eq!(scroll_buttons(19.0, 19.0), Vec::<(u8, u32)>::new());
        assert_eq!(scroll_buttons(-40.0, 40.0), vec![(5, 1), (6, 1)]);
    }

    #[test]
    fn scrollbar_values_are_clamped_to_the_reported_range() {
        assert_eq!(clamp_scroll_value(90.0, 80.0, 0.0, 100.0), 100.0);
        assert_eq!(clamp_scroll_value(40.0, -80.0, 0.0, 100.0), 0.0);
    }
}
