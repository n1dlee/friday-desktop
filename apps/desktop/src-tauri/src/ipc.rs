use std::{sync::atomic::Ordering, time::Duration};

use friday_adapters::windows_host::{WindowsAudio, WindowsSpeech};
use friday_engine::{Failure, HistoryEntry, Intent, RequestAck, SessionStatus, Settings, Snapshot};
use secrecy::SecretString;
use serde::Serialize;
use tauri::{AppHandle, State, WebviewWindow};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};
use uuid::Uuid;

use crate::{host::Host, updates};

#[derive(Serialize)]
pub(crate) struct Bootstrap {
    protocol: u32,
    version: &'static str,
    snapshot: Snapshot,
    settings: Settings,
    key_present: bool,
    key_warning: Option<Failure>,
    shortcut_registered: bool,
    updates_configured: bool,
}

fn snapshot(host: &Host) -> Snapshot {
    host.engine.snapshot()
}

pub(crate) fn local(window: &WebviewWindow) -> Result<(), Failure> {
    if window.label() == "main" {
        Ok(())
    } else {
        Err(Failure::Locked)
    }
}

pub(crate) fn interactive(window: &WebviewWindow, host: &Host) -> Result<(), Failure> {
    local(window)?;
    if host.closing.load(Ordering::Acquire) {
        return Err(Failure::Stopped);
    }
    host.engine.ensure_unlocked()?;
    if !window.is_focused().unwrap_or(false) {
        return Err(Failure::Locked);
    }
    Ok(())
}

async fn boot(host: &Host) -> Bootstrap {
    let key = if host.engine.ensure_unlocked().is_ok() {
        host.credentials.present().await
    } else {
        Ok(false)
    };
    let session = snapshot(host);
    let (key_present, key_warning) = if session.session == SessionStatus::Unlocked {
        match key {
            Ok(value) => (value, None),
            Err(error) => (false, Some(error)),
        }
    } else {
        (false, None)
    };
    Bootstrap {
        protocol: 1,
        version: env!("CARGO_PKG_VERSION"),
        snapshot: session,
        settings: host.engine.settings(),
        key_present,
        key_warning,
        shortcut_registered: host.shortcut_id.load(Ordering::Acquire) != 0,
        updates_configured: updates::config().is_some(),
    }
}

#[tauri::command]
pub(crate) async fn bootstrap(
    window: WebviewWindow,
    state: State<'_, Host>,
) -> Result<Bootstrap, Failure> {
    local(&window)?;
    Ok(boot(&state).await)
}

#[tauri::command]
pub(crate) async fn observe_state(
    window: WebviewWindow,
    state: State<'_, Host>,
    since: u64,
) -> Result<Snapshot, Failure> {
    local(&window)?;
    let _permit = state.observers.try_acquire().map_err(|_| Failure::Busy)?;
    let mut changes = state.engine.subscribe();
    changes.borrow_and_update();
    let current = snapshot(&state);
    if current.revision != since {
        return Ok(current);
    }
    let _ = tokio::time::timeout(Duration::from_millis(1500), changes.changed()).await;
    Ok(snapshot(&state))
}

#[tauri::command]
pub(crate) async fn submit(
    window: WebviewWindow,
    state: State<'_, Host>,
    request_id: Uuid,
    intent: Intent,
) -> Result<RequestAck, Failure> {
    let _guard = state.mutations.lock().await;
    interactive(&window, &state)?;
    state.engine.submit(request_id, intent).await
}

#[tauri::command]
pub(crate) async fn finish_capture(
    window: WebviewWindow,
    state: State<'_, Host>,
    turn_id: Uuid,
) -> Result<(), Failure> {
    interactive(&window, &state)?;
    state.engine.finish_capture(turn_id)
}

#[tauri::command]
pub(crate) async fn cancel_turn(
    window: WebviewWindow,
    state: State<'_, Host>,
    turn_id: Uuid,
) -> Result<(), Failure> {
    local(&window)?;
    state.engine.cancel(turn_id)
}

#[tauri::command]
pub(crate) async fn update_settings(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, Host>,
    settings: Settings,
) -> Result<Bootstrap, Failure> {
    let _guard = state.mutations.lock().await;
    interactive(&window, &state)?;
    state.idle()?;
    settings.validate()?;
    let requested = settings
        .shortcut
        .parse::<Shortcut>()
        .map_err(|_| Failure::InvalidInput)?;
    let id = state.shortcut_id.load(Ordering::Acquire);
    let previous = state.engine.settings().shortcut.parse::<Shortcut>().ok();
    let changed = id != requested.id();
    if changed {
        app.global_shortcut()
            .register(requested)
            .map_err(|_| Failure::ShortcutUnavailable)?;
    }
    if let Err(error) = state.engine.update_settings(settings).await {
        if changed {
            let _ = app.global_shortcut().unregister(requested);
        }
        return Err(error);
    }
    state.shortcut_id.store(requested.id(), Ordering::Release);
    if changed && let Some(previous) = previous.filter(|old| old.id() == id) {
        let _ = app.global_shortcut().unregister(previous);
    }
    Ok(boot(&state).await)
}

#[tauri::command]
pub(crate) async fn set_groq_key(
    window: WebviewWindow,
    state: State<'_, Host>,
    key: String,
) -> Result<(), Failure> {
    let key = SecretString::from(key);
    let _guard = state.mutations.lock().await;
    interactive(&window, &state)?;
    state.idle()?;
    state.credentials.set(key).await
}

#[tauri::command]
pub(crate) async fn delete_groq_key(
    window: WebviewWindow,
    state: State<'_, Host>,
) -> Result<(), Failure> {
    let _guard = state.mutations.lock().await;
    interactive(&window, &state)?;
    state.idle()?;
    state.credentials.delete().await
}

#[tauri::command]
pub(crate) async fn load_history(
    window: WebviewWindow,
    state: State<'_, Host>,
) -> Result<Vec<HistoryEntry>, Failure> {
    interactive(&window, &state)?;
    state.engine.history().await
}

#[tauri::command]
pub(crate) async fn clear_history(
    window: WebviewWindow,
    state: State<'_, Host>,
) -> Result<(), Failure> {
    let _guard = state.mutations.lock().await;
    interactive(&window, &state)?;
    state.idle()?;
    state.engine.clear_history().await
}

#[derive(Serialize)]
pub(crate) struct Diagnostics {
    app_version: &'static str,
    protocol: u32,
    session: SessionStatus,
    shortcut_registered: bool,
    cloud_enabled: bool,
    last_phase: friday_engine::Phase,
    microphone_available: Option<bool>,
    local_voice_available: Option<bool>,
    last_failure: Option<Failure>,
    unresolved_actions: i64,
    updates_configured: bool,
}

#[tauri::command]
pub(crate) async fn diagnostics(
    window: WebviewWindow,
    state: State<'_, Host>,
) -> Result<Diagnostics, Failure> {
    interactive(&window, &state)?;
    let current = snapshot(&state);
    let _probe = state.mutations.try_lock().map_err(|_| Failure::Busy)?;
    let budget = Duration::from_secs(1);
    let (unresolved_actions, microphone, voice) = tokio::join!(
        state.store.unresolved_actions(),
        tokio::time::timeout(budget, WindowsAudio::input_available()),
        tokio::time::timeout(
            budget,
            WindowsSpeech::voice_available(state.engine.settings().locale)
        ),
    );
    let unresolved_actions = unresolved_actions?;
    state.engine.ensure_unlocked()?;
    Ok(Diagnostics {
        app_version: env!("CARGO_PKG_VERSION"),
        protocol: 1,
        session: current.session,
        shortcut_registered: state.shortcut_id.load(Ordering::Acquire) != 0,
        cloud_enabled: state.engine.settings().cloud_enabled,
        last_phase: current.phase,
        microphone_available: microphone.ok(),
        local_voice_available: voice.ok(),
        last_failure: current.failure,
        unresolved_actions,
        updates_configured: updates::config().is_some(),
    })
}
