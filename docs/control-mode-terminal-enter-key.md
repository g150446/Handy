# Control Mode: Terminal Enter Key Fix

## Problem

In control mode, saying "press Enter" failed silently in WezTerm (and potentially other GPU-based terminal emulators).

## Root Causes

### 1. Process name vs. bundle name mismatch

`get_frontmost_app_name()` queries System Events for the **process name** of the frontmost app. For WezTerm on macOS, this returns `"wezterm-gui"` — the binary inside the bundle — not `"WezTerm"` (the bundle display name).

The original Enter key script used:

```applescript
tell application "wezterm-gui" to activate
```

AppleScript's `tell application X to activate` resolves `X` through **LaunchServices by bundle name**, not process name. Since no bundle is named `"wezterm-gui"`, this call fails silently. The control window remains focused, so the subsequent keystroke goes nowhere.

### 2. `key code 36` not reliably received by GPU-based terminals

`key code 36` sends a low-level hardware keycode via System Events. GPU-accelerated terminals (WezTerm, Alacritty, Kitty, etc.) may not receive this through their input pipeline. `keystroke return` — a higher-level synthetic keystroke — is more reliably delivered.

## Fix (`src-tauri/src/control.rs`)

### `TERMINAL_APP_NAMES` — added `"wezterm-gui"`

```rust
const TERMINAL_APP_NAMES: &[&str] = &[
    "Terminal",
    "iTerm2",
    "kitty",
    "Alacritty",
    "WezTerm",
    "wezterm-gui",   // process name returned by System Events for WezTerm
    "Warp",
    "Hyper",
    "Ghostty",
];
```

WezTerm may report as either `"WezTerm"` or `"wezterm-gui"` depending on macOS version and how the app was launched. Both are covered.

### `execute_enter_key_action` — terminal-aware activation + keystroke

For terminal apps, instead of `tell application X to activate` (bundle name lookup), we now use System Events to focus **by process name**:

```applescript
tell application "System Events"
    set frontmost of (first process whose name is "wezterm-gui") to true
end tell
delay 0.3
tell application "System Events" to keystroke return
```

For all other apps (browsers, editors, etc.) the original approach is preserved:

```applescript
tell application "Safari" to activate
delay 0.3
tell application "System Events" to key code 36
```

This is consistent with how `execute_undo_last_input` and `execute_replace_input` already handle terminal apps (they also branch on `is_terminal_app()` for Ctrl+U vs Cmd+Z and Cmd+V vs Ctrl+V).

## Extending to Other Terminals

If a new terminal emulator fails to receive the Enter key:

1. Find its process name: open the app, then run in Terminal:
   ```bash
   osascript -e 'tell application "System Events" to name of (first application process whose frontmost is true)'
   ```
2. Add the returned name to `TERMINAL_APP_NAMES` in `control.rs`.

The `is_terminal_app()` check is case-insensitive, so casing doesn't matter.

## Related Functions

| Function | File | Notes |
|---|---|---|
| `get_frontmost_app_name()` | `control.rs:449` | Returns process name via System Events |
| `is_terminal_app()` | `control.rs:442` | Case-insensitive lookup in `TERMINAL_APP_NAMES` |
| `execute_enter_key_action()` | `control.rs:621` | Enter key — now terminal-aware |
| `execute_undo_last_input()` | `control.rs:466` | Ctrl+U for terminals, Cmd+Z elsewhere |
| `execute_replace_input()` | `control.rs:551` | Ctrl+V for terminals, Cmd+V elsewhere |
