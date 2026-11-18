use std::{sync::Arc, sync::atomic::Ordering, time::Duration};

use friday_engine::Failure;
use reqwest::redirect::Policy;
use serde::Serialize;
use tauri::{AppHandle, State, WebviewWindow};
use tauri_plugin_updater::UpdaterExt;
use tokio::sync::Notify;

use crate::{host::Host, ipc};

const MAX_INSTALL_BYTES: usize = 150 * 1024 * 1024;

pub(crate) fn config() -> Option<(&'static str, &'static str)> {
    let endpoint = option_env!("FRIDAY_UPDATE_ENDPOINT")?;
    let key = option_env!("FRIDAY_UPDATER_PUBLIC_KEY")?;
    if !endpoint.starts_with("https://") || key.trim().is_empty() {
        return None;
    }
    Some((endpoint, key))
}

#[derive(Serialize)]
pub(crate) struct UpdateInfo {
    version: String,
}

#[tauri::command]
pub(crate) async fn check_update(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, Host>,
) -> Result<Option<UpdateInfo>, Failure> {
    ipc::interactive(&window, &state)?;
    let (endpoint, key) = config().ok_or(Failure::UpdateUnavailable)?;
    let mut selected = state.update.try_lock().map_err(|_| Failure::Busy)?;
    *selected = None;
    let endpoint = endpoint.parse().map_err(|_| Failure::UpdateUnavailable)?;
    let updater = app
        .updater_builder()
        .pubkey(key)
        .endpoints(vec![endpoint])
        .map_err(|_| Failure::UpdateUnavailable)?
        .timeout(Duration::from_secs(25))
        .configure_client(|builder| {
            builder.redirect(Policy::custom(|attempt| {
                if attempt.url().scheme() == "https" && attempt.previous().len() < 5 {
                    attempt.follow()
                } else {
                    attempt.error("insecure or excessive update redirects")
                }
            }))
        })
        .build()
        .map_err(|_| Failure::UpdateFailed)?;
    let offer = updater.check().await.map_err(|_| Failure::UpdateFailed)?;
    if offer
        .as_ref()
        .is_some_and(|offer| offer.download_url.scheme() != "https")
    {
        return Err(Failure::UpdateFailed);
    }
    *selected = offer;
    Ok(selected.as_ref().map(|offer| UpdateInfo {
        version: offer.version.clone(),
    }))
}

#[tauri::command]
pub(crate) async fn install_update(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, Host>,
    version: String,
) -> Result<(), Failure> {
    ipc::interactive(&window, &state)?;
    let mut selected = state.update.try_lock().map_err(|_| Failure::Busy)?;
    let offer = selected
        .take()
        .filter(|offer| offer.version == version)
        .ok_or(Failure::UpdateUnavailable)?;
    // Limit buffering before the updater verifies the signed bytes.
    let overflow = Arc::new(Notify::new());
    let limit = overflow.clone();
    let mut received = 0_usize;
    let download = offer.download(
        move |size, total| {
            received = received.saturating_add(size);
            if received > MAX_INSTALL_BYTES
                || total.is_some_and(|bytes| bytes > MAX_INSTALL_BYTES as u64)
            {
                limit.notify_one();
            }
        },
        || {},
    );
    let bytes = tokio::select! {
        biased;
        _ = overflow.notified() => return Err(Failure::UpdateFailed),
        result = tokio::time::timeout(Duration::from_secs(90), download) => {
            result.map_err(|_| Failure::Timeout)?.map_err(|_| Failure::UpdateFailed)?
        }
    };
    if bytes.len() > MAX_INSTALL_BYTES {
        return Err(Failure::UpdateFailed);
    }
    drop(selected);
    {
        let _guard = state.mutations.lock().await;
        ipc::interactive(&window, &state)?;
        state.idle()?;
        state.closing.store(true, Ordering::Release);
    }
    state.shutdown().await;
    if offer.install(bytes).is_err() {
        tracing::error!("verified update could not start installation");
        app.restart();
    }
    Ok(())
}
