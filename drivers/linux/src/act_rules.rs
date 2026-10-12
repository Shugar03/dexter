pub const PRESS_VERBS: [&str; 5] = ["click", "activate", "press", "toggle", "jump"];

pub fn press_action_index(names: &[String]) -> Option<i32> {
    PRESS_VERBS.iter().find_map(|verb| {
        names
            .iter()
            .position(|name| name.trim().eq_ignore_ascii_case(verb))
            .and_then(|index| i32::try_from(index).ok())
    })
}

pub fn numeric_value(value: &str) -> Option<f64> {
    value
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
}

#[cfg(test)]
mod tests {
    use super::{numeric_value, press_action_index};

    #[test]
    fn press_action_prefers_verb_order_over_list_order() {
        let names = ["jump", " Toggle ", "activate"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        assert_eq!(press_action_index(&names), Some(2));
    }

    #[test]
    fn press_action_rejects_non_press_verbs_and_empty_lists() {
        for name in ["release", "expand or contract", "menu", "unknown"] {
            assert_eq!(press_action_index(&[name.into()]), None, "{name}");
        }
        assert_eq!(press_action_index(&[]), None);
    }

    #[test]
    fn numeric_value_trims_and_rejects_non_finite_numbers() {
        assert_eq!(numeric_value(" 12.5 "), Some(12.5));
        assert_eq!(numeric_value("-3"), Some(-3.0));
        for value in ["", "not a number", "NaN", "inf", "-Infinity"] {
            assert_eq!(numeric_value(value), None, "{value}");
        }
    }
}
