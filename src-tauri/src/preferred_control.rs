//! Preferred control mode: Desktop Control vs Harbor Control.
//!
//! Drives BLE double-tap / shortcut toggle and local voice mode-switch phrases.

use tauri::AppHandle;

use crate::settings::{self, PreferredControlMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeSwitchIntent {
    Harbor,
    Desktop,
    Normal,
}

/// Persist which control surface BLE / preferred shortcut should toggle.
pub fn set_preferred(app: &AppHandle, mode: PreferredControlMode) {
    let mut settings = settings::get_settings(app);
    if settings.preferred_control_mode == mode {
        return;
    }
    settings.preferred_control_mode = mode;
    settings::write_settings(app, settings);
    log::info!("preferred_control_mode set to {:?}", mode);
}

pub fn get_preferred(app: &AppHandle) -> PreferredControlMode {
    settings::get_settings(app).preferred_control_mode
}

/// Toggle the user-preferred control surface (Harbor or Desktop).
/// On deactivate, shows the normal-input overlay.
pub fn toggle_preferred(app: &AppHandle) -> Result<(), String> {
    match get_preferred(app) {
        PreferredControlMode::Harbor => {
            let snapshot = crate::harbor_control::toggle(app)?;
            log::info!(
                "Preferred Harbor Control toggled: active={} paired={}",
                snapshot.active,
                snapshot.paired
            );
            if !snapshot.active {
                crate::overlay::show_normal_input_overlay(app);
            }
            Ok(())
        }
        PreferredControlMode::Desktop => {
            let snapshot = crate::control::toggle_mode(app)?;
            log::info!(
                "Preferred Desktop Control toggled: active={}",
                snapshot.active
            );
            if !snapshot.active {
                crate::overlay::show_normal_input_overlay(app);
            }
            Ok(())
        }
    }
}

/// Switch into Harbor Control and remember it as preferred.
pub fn activate_harbor(app: &AppHandle) -> Result<(), String> {
    set_preferred(app, PreferredControlMode::Harbor);
    let _ = crate::control::deactivate_mode(app);
    crate::harbor_control::begin_session(app)?;
    Ok(())
}

/// Switch into Desktop Control and remember it as preferred.
pub fn activate_desktop(app: &AppHandle) -> Result<(), String> {
    set_preferred(app, PreferredControlMode::Desktop);
    let _ = crate::harbor_control::deactivate(app);
    crate::control::set_mode_active(app, true)?;
    Ok(())
}

/// Switch back to normal input (both control surfaces off). Does not change preferred.
pub fn activate_normal(app: &AppHandle) -> Result<(), String> {
    let _ = crate::harbor_control::deactivate(app);
    let _ = crate::control::deactivate_mode(app);
    crate::overlay::show_normal_input_overlay(app);
    Ok(())
}

/// Normalize transcript for phrase matching.
fn normalize_phrase(text: &str) -> String {
    text.chars()
        .filter(|c| {
            !c.is_whitespace()
                && !matches!(
                    c,
                    '。' | '、' | '，' | ',' | '.' | '!' | '？' | '?' | '：' | ':' | '；' | ';'
                )
        })
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Detect local mode-switch voice intents (no LLM).
pub fn match_mode_switch_intent(text: &str) -> Option<ModeSwitchIntent> {
    let n = normalize_phrase(text);
    if n.is_empty() {
        return None;
    }

    // Longer / more specific phrases first.
    const HARBOR: &[&str] = &[
        "terminalharbor",
        "terminalharborcontrol",
        "harborcontrol",
        "harbormode",
        "switchtoharbor",
        "gotoharbor",
        "openharbor",
        "ターミナルハーバー",
        "ハーバーコントロール",
        "ハーバーモード",
        "ハーバーに切替",
        "ハーバーに切り替え",
        "ハーバーへ",
        "ハーバー",
        "harbor",
    ];
    const DESKTOP: &[&str] = &[
        "desktopcontrol",
        "desktopmode",
        "desktopcontrolmode",
        "switchtodesktop",
        "gotodesktop",
        "agentcontrol",
        "agentmode",
        "controlmode",
        "デスクトップ操作モード",
        "デスクトップコントロール",
        "デスクトップ操作",
        "デスクトップモード",
        "デスクトップに切替",
        "デスクトップに切り替え",
        "デスクトップへ",
        "デスクトップ",
        "desktop",
    ];
    const NORMAL: &[&str] = &[
        "normalinput",
        "normalmode",
        "exitcontrol",
        "exitmode",
        "通常入力モード",
        "通常入力",
        "通常モード",
        "モード終了",
        "コントロール終了",
        "終了",
        "normal",
    ];

    for p in HARBOR {
        if n.contains(p) || n == *p {
            // Avoid matching bare "終了" style collisions; harbor phrases are distinct.
            return Some(ModeSwitchIntent::Harbor);
        }
    }
    for p in DESKTOP {
        if n.contains(p) || n == *p {
            return Some(ModeSwitchIntent::Desktop);
        }
    }
    for p in NORMAL {
        if n == *p || n.contains(p) {
            // Require short utterances for vague words like 終了 / normal
            if *p == "終了" || *p == "normal" {
                if n == *p || n.ends_with(p) && n.chars().count() <= p.chars().count() + 4 {
                    return Some(ModeSwitchIntent::Normal);
                }
                continue;
            }
            return Some(ModeSwitchIntent::Normal);
        }
    }
    None
}

/// Apply a detected mode-switch intent. Returns true if handled.
pub fn apply_mode_switch_intent(app: &AppHandle, intent: ModeSwitchIntent) -> Result<bool, String> {
    match intent {
        ModeSwitchIntent::Harbor => {
            log::info!("Voice mode switch → Harbor Control");
            activate_harbor(app)?;
            Ok(true)
        }
        ModeSwitchIntent::Desktop => {
            log::info!("Voice mode switch → Desktop Control");
            activate_desktop(app)?;
            Ok(true)
        }
        ModeSwitchIntent::Normal => {
            log::info!("Voice mode switch → normal input");
            activate_normal(app)?;
            Ok(true)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_harbor_phrases() {
        assert_eq!(
            match_mode_switch_intent("ハーバーモード"),
            Some(ModeSwitchIntent::Harbor)
        );
        assert_eq!(
            match_mode_switch_intent("harbor mode"),
            Some(ModeSwitchIntent::Harbor)
        );
        assert_eq!(
            match_mode_switch_intent("ターミナルハーバー"),
            Some(ModeSwitchIntent::Harbor)
        );
    }

    #[test]
    fn matches_desktop_phrases() {
        assert_eq!(
            match_mode_switch_intent("デスクトップ操作"),
            Some(ModeSwitchIntent::Desktop)
        );
        assert_eq!(
            match_mode_switch_intent("desktop control"),
            Some(ModeSwitchIntent::Desktop)
        );
    }

    #[test]
    fn ignores_unrelated() {
        assert_eq!(match_mode_switch_intent("クロードの方"), None);
        assert_eq!(match_mode_switch_intent("undo the last paste"), None);
    }
}
