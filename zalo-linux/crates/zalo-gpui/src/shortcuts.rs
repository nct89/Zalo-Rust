//! Global screenshot shortcuts via `org.freedesktop.portal.GlobalShortcuts` + in-app key fallback.

use std::collections::HashMap;

use futures::StreamExt;
use gpui::{AnyWindowHandle, Context, KeyDownEvent, Window};
use zalo_shell::{shortcut_id, shortcut_slot, GlobalShortcuts, ShortcutEvent, ShortcutSpec};

use crate::shell::ShellView;

/// Portal slot ids (`zalo_shell::shortcut_id` suffixes the trigger).
pub(crate) const SLOTS: [&str; 2] = ["shot-with-window", "shot-without-window"];
/// Pref key for slot 0 ("Screenshot with Zalo window").
pub(crate) const PREF_WITH: &str = "shotWithWindow";
/// Pref key for slot 1 ("Screenshot WITHOUT Zalo window").
pub(crate) const PREF_WITHOUT: &str = "shotWithoutWindow";
/// Default trigger for slot 0.
pub(crate) const DEFAULT_WITH: &str = "CTRL+ALT+s";
/// Default trigger for slot 1.
pub(crate) const DEFAULT_WITHOUT: &str = "CTRL+ALT+SHIFT+s";

const PRESET_TRIGGERS: &[&str] = &[
    "CTRL+ALT+s",
    "CTRL+ALT+SHIFT+s",
    "CTRL+ALT+a",
    "CTRL+ALT+x",
    "CTRL+SHIFT+F5",
];

/// GlobalShortcuts portal handle + bound status held on [`ShellView`].
#[derive(Default)]
pub(crate) struct ShortcutState {
    service: Option<GlobalShortcuts>,
    pub started: bool,
    pub bound: HashMap<String, String>,
    pub error: Option<String>,
    sent_triggers: Option<(String, String)>,
}

/// Formats `CTRL+ALT+SHIFT+s` -> `Ctrl + Alt + Shift + S` for settings labels.
#[must_use]
pub(crate) fn format_trigger(trigger: &str) -> String {
    trigger
        .split('+')
        .filter(|p| !p.is_empty())
        .map(|part| match part.to_ascii_uppercase().as_str() {
            "CTRL" => "Ctrl".to_owned(),
            "ALT" => "Alt".to_owned(),
            "SHIFT" => "Shift".to_owned(),
            _ => part.to_ascii_uppercase(),
        })
        .collect::<Vec<_>>()
        .join(" + ")
}

/// Builds the two [`ShortcutSpec`] items for XDG GlobalShortcuts registration.
#[must_use]
pub(crate) fn specs_for(with_win: &str, without_win: &str) -> Vec<ShortcutSpec> {
    [
        (SLOTS[0], "Chụp màn hình kèm với cửa sổ Zalo", with_win),
        (SLOTS[1], "Chụp màn hình KHÔNG kèm cửa sổ Zalo", without_win),
    ]
    .into_iter()
    .map(|(slot, description, trigger)| ShortcutSpec {
        id: shortcut_id(slot, trigger),
        description: description.to_owned(),
        trigger: trigger.to_owned(),
    })
    .collect()
}

/// Picks the next non-colliding preset trigger when cycling in Settings.
#[must_use]
pub(crate) fn next_preset_trigger(current: &str, other: &str) -> &'static str {
    let idx = PRESET_TRIGGERS
        .iter()
        .position(|p| p.eq_ignore_ascii_case(current))
        .unwrap_or(0);
    for offset in 1..=PRESET_TRIGGERS.len() {
        let candidate = PRESET_TRIGGERS[(idx + offset) % PRESET_TRIGGERS.len()];
        if !candidate.eq_ignore_ascii_case(other) {
            return candidate;
        }
    }
    PRESET_TRIGGERS[0]
}

/// Returns `true` if `event` matches an XDG trigger string such as `CTRL+ALT+SHIFT+s`.
#[must_use]
pub(crate) fn matches_keystroke(trigger: &str, event: &KeyDownEvent) -> bool {
    let (mut need_ctrl, mut need_alt, mut need_shift) = (false, false, false);
    let mut key_part = None;
    for part in trigger.split('+').filter(|p| !p.is_empty()) {
        match part.to_ascii_uppercase().as_str() {
            "CTRL" => need_ctrl = true,
            "ALT" => need_alt = true,
            "SHIFT" => need_shift = true,
            _ => key_part = Some(part),
        }
    }
    let Some(expected_key) = key_part else {
        return false;
    };
    let m = &event.keystroke.modifiers;
    (need_ctrl || need_alt)
        && m.control == need_ctrl
        && m.alt == need_alt
        && m.shift == need_shift
        && !m.platform
        && !m.function
        && event.keystroke.key.eq_ignore_ascii_case(expected_key)
}

impl ShellView {
    /// Returns `(slot0_trigger, slot1_trigger)` from saved prefs with defaults.
    #[must_use]
    pub(crate) fn shot_triggers(&self) -> (String, String) {
        let with = self.prefs.str(PREF_WITH, DEFAULT_WITH);
        let without = self.prefs.str(PREF_WITHOUT, DEFAULT_WITHOUT);
        let with = if with.is_empty() {
            DEFAULT_WITH.to_owned()
        } else {
            with
        };
        let without = if without.is_empty() || without.eq_ignore_ascii_case(&with) {
            DEFAULT_WITHOUT.to_owned()
        } else {
            without
        };
        (with, without)
    }

    /// Ensures the `zalo_shell::GlobalShortcuts` session is bound for the main shell window.
    pub(crate) fn start_shortcuts(&mut self, window: &Window, cx: &mut Context<Self>) {
        if cfg!(test) || self.solo.is_some() {
            return;
        }
        let pair = self.shot_triggers();
        if self.shortcuts.sent_triggers.as_ref() == Some(&pair) {
            return;
        }
        self.shortcuts.sent_triggers = Some(pair.clone());
        let specs = specs_for(&pair.0, &pair.1);
        if let Some(svc) = &self.shortcuts.service {
            svc.rebind(specs);
            return;
        }
        self.shortcuts.started = true;
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<ShortcutEvent>();
        self.shortcuts.service = Some(GlobalShortcuts::start(
            zalo_shell::GPUI_APP_ID,
            specs,
            move |event| {
                let _ = tx.unbounded_send(event);
            },
        ));
        let wh = window.window_handle();
        cx.spawn(async move |this, cx| {
            while let Some(ev) = rx.next().await {
                let alive = this.update(cx, |view, cx| view.on_shortcut_event(ev, wh, cx));
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn on_shortcut_event(
        &mut self,
        event: ShortcutEvent,
        wh: AnyWindowHandle,
        cx: &mut Context<Self>,
    ) {
        match event {
            ShortcutEvent::Bound(list) => {
                self.shortcuts.error = None;
                self.shortcuts.bound.clear();
                for (id, trigger) in list {
                    if let Some(slot) = shortcut_slot(&id, &SLOTS) {
                        self.shortcuts.bound.insert(slot.to_owned(), trigger);
                    }
                }
            }
            ShortcutEvent::Unavailable(why) => {
                self.shortcuts.error = Some(why);
            }
            ShortcutEvent::Activated(id) => {
                if let Some(slot_name) = shortcut_slot(&id, &SLOTS) {
                    let slot = usize::from(slot_name == SLOTS[1]);
                    self.trigger_shot_slot(slot, Some(wh), cx);
                }
            }
        }
        cx.notify();
    }

    /// Triggers slot 0 (with Zalo window) or slot 1 (minimizing Zalo first).
    pub(crate) fn trigger_shot_slot(
        &mut self,
        slot: usize,
        window: Option<AnyWindowHandle>,
        cx: &mut Context<Self>,
    ) {
        if self.locked {
            return;
        }
        self.capture_screenshot_mode(slot == 1, window, cx);
    }

    /// Cycles the configured screenshot shortcut for slot `0` or `1` and rebinds.
    pub(crate) fn cycle_shot_trigger(&mut self, slot: usize) {
        let (with, without) = self.shot_triggers();
        if slot == 0 {
            let next = next_preset_trigger(&with, &without);
            self.prefs
                .set(PREF_WITH, serde_json::Value::String(next.to_owned()));
        } else {
            let next = next_preset_trigger(&without, &with);
            self.prefs
                .set(PREF_WITHOUT, serde_json::Value::String(next.to_owned()));
        }
    }

    /// Status line painted under Settings -> Utilities for `GlobalShortcuts`.
    /// Never includes a portal error string.
    #[must_use]
    pub(crate) fn shortcut_portal_status(&self) -> String {
        shortcut_status_text(!self.shortcuts.bound.is_empty()).to_owned()
    }
}

/// Sentence under "Screenshot Shortcuts". `bound` is a live GlobalShortcuts session.
#[must_use]
pub(crate) fn shortcut_status_text(bound: bool) -> &'static str {
    if bound {
        "Phím tắt có hiệu lực trên toàn hệ thống."
    } else {
        "Phím tắt chỉ dùng được khi cửa sổ Zalo đang mở."
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_formatting_specs_and_cycles() {
        assert_eq!(format_trigger("CTRL+ALT+s"), "Ctrl + Alt + S");
        assert_eq!(format_trigger("CTRL+ALT+SHIFT+s"), "Ctrl + Alt + Shift + S");
        let specs = specs_for("CTRL+ALT+s", "CTRL+ALT+a");
        assert_eq!(specs.len(), 2);
        assert_eq!(shortcut_slot(&specs[0].id, &SLOTS), Some(SLOTS[0]));
        assert_eq!(shortcut_slot(&specs[1].id, &SLOTS), Some(SLOTS[1]));

        let next = next_preset_trigger("CTRL+ALT+s", "CTRL+ALT+SHIFT+s");
        assert_eq!(next, "CTRL+ALT+a");
    }

    #[test]
    fn shortcut_status_never_mentions_a_bus_error() {
        let on = shortcut_status_text(true);
        let off = shortcut_status_text(false);
        assert!(on.contains("toàn hệ thống"));
        assert!(off.contains("cửa sổ Zalo"));
        assert!(!on.contains("DBus") && !off.contains("DBus"));
        assert!(!off.contains("org.freedesktop"));
    }
}
