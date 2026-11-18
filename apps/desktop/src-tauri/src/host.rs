use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::Duration,
};

use friday_adapters::{
    Groq, SqliteStore,
    windows_host::{
        SessionMonitor, WindowsActions, WindowsAudio, WindowsCredentials, WindowsSpeech,
    },
};
use friday_engine::{Engine, EngineHandle, Failure, Intent, Phase, Ports, SessionStatus};
use tauri::{
    AppHandle, Manager, RunEvent, WindowEvent,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
use tokio::sync::{Mutex as AsyncMutex, Semaphore};
use uuid::Uuid;

use crate::{diagnostics, ipc, updates};

pub(crate) struct Host {
    pub(crate) engine: EngineHandle,
    pub(crate) credentials: Arc<WindowsCredentials>,
    pub(crate) store: Arc<SqliteStore>,
    pub(crate) mutations: AsyncMutex<()>,
    pub(crate) observers: Semaphore,
    pub(crate) shortcut_id: AtomicU32,
    pub(crate) closing: AtomicBool,
    pub(crate) update: AsyncMutex<Option<tauri_plugin_updater::Update>>,
    engine_task: AsyncMutex<Option<Engine>>,
    monitor: Mutex<Option<SessionMonitor>>,
}

impl Host {
    pub(crate) fn idle(&self) -> Result<(), Failure> {
        if self.closing.load(Ordering::Acquire) {
            return Err(Failure::Stopped);
        }
        self.engine.ensure_unlocked()?;
        if self.engine.snapshot().phase.active() {
            return Err(Failure::Busy);
        }
        Ok(())
    }

    pub(crate) async fn shutdown(&self) {
        self.engine.cancel_current();
        if let Some(engine) = self.engine_task.lock().await.take() {
            let _ = tokio::time::timeout(Duration::from_secs(5), engine.shutdown()).await;
        }
        if let Ok(mut monitor) = self.monitor.lock() {
            monitor.take();
        }
    }
}

pub(crate) fn show(app: &AppHandle) -> bool {
    let Some(window) = app.get_webview_window("main") else {
        return false;
    };
    window.show().is_ok() && window.set_focus().is_ok()
}

fn shortcut(app: &AppHandle, selected: &Shortcut) {
    let Some(host) = app.try_state::<Host>() else {
        return;
    };
    if host.shortcut_id.load(Ordering::Acquire) != selected.id()
        || host.closing.load(Ordering::Acquire)
        || host.engine.ensure_unlocked().is_err()
        || !show(app)
    {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let host = app.state::<Host>();
        let _guard = host.mutations.lock().await;
        if host.closing.load(Ordering::Acquire) {
            return;
        }
        let snapshot = host.engine.snapshot();
        match (snapshot.phase, snapshot.turn_id) {
            (Phase::Capturing, Some(id)) => {
                let _ = host.engine.finish_capture(id);
            }
            (phase, Some(id)) if phase.active() => {
                let _ = host.engine.cancel(id);
            }
            _ if host.engine.settings().cloud_enabled => {
                let _ = host
                    .engine
                    .submit(Uuid::new_v4(), Intent::StartCapture)
                    .await;
            }
            _ => {}
        }
    });
}

fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let directory = app.path().app_local_data_dir()?;
    std::fs::create_dir_all(&directory)?;
    diagnostics::init(&directory);
    let (store, settings) = SqliteStore::open_profile(&directory)?;
    let store = Arc::new(store);
    let credentials = Arc::new(WindowsCredentials::default());
    let cloud = Groq::new(credentials.clone()).ok();
    let ports = Ports {
        platform: Arc::new(WindowsActions::default()),
        store: store.clone(),
        audio: Some(Arc::new(WindowsAudio)),
        cloud: cloud.map(|provider| Arc::new(provider) as Arc<dyn friday_engine::Cloud>),
        speech: Some(Arc::new(WindowsSpeech)),
    };
    let configured_shortcut = settings.shortcut.clone();
    let engine = tauri::async_runtime::block_on(async {
        Engine::start(ports, settings, SessionStatus::Unknown)
    });
    let handle = engine.handle();
    let monitor = SessionMonitor::start(handle.clone()).ok();
    if monitor.is_none() {
        tracing::warn!("Windows session monitor did not start");
    }
    app.manage(Host {
        engine: handle,
        credentials,
        store,
        mutations: AsyncMutex::new(()),
        observers: Semaphore::new(1),
        shortcut_id: AtomicU32::new(0),
        closing: AtomicBool::new(false),
        update: AsyncMutex::new(None),
        engine_task: AsyncMutex::new(Some(engine)),
        monitor: Mutex::new(monitor),
    });

    let show_item = MenuItem::with_id(app, "show", "Friday", true, None::<&str>)?;
    let exit_label = match app.state::<Host>().engine.settings().locale {
        friday_core::Locale::Ru => "Выйти",
        friday_core::Locale::En => "Quit",
    };
    let close_item = MenuItem::with_id(app, "quit", exit_label, true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show_item, &close_item])?;
    let mut tray = TrayIconBuilder::with_id("friday")
        .tooltip("Friday Desktop")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => {
                show(app);
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) {
                show(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;

    // Register off the UI thread: the plugin dispatches registration to that thread.
    let manager = app.handle().clone();
    tauri::async_runtime::spawn(async move {
        let host = manager.state::<Host>();
        let _guard = host.mutations.lock().await;
        if let Ok(parsed) = configured_shortcut.parse::<Shortcut>()
            && manager.global_shortcut().register(parsed).is_ok()
        {
            host.shortcut_id.store(parsed.id(), Ordering::Release);
        }
    });
    Ok(())
}

pub(crate) fn run() {
    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            show(app);
        }))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, selected, event| {
                    if event.state == ShortcutState::Pressed {
                        shortcut(app, selected);
                    }
                })
                .build(),
        );
    if updates::config().is_some() {
        builder = builder.plugin(tauri_plugin_updater::Builder::new().build());
    }
    let app = builder
        .setup(setup)
        .on_window_event(|window, event| {
            if window.label() == "main"
                && let WindowEvent::CloseRequested { api, .. } = event
            {
                let host = window.state::<Host>();
                if !host.closing.load(Ordering::Acquire) {
                    api.prevent_close();
                    host.engine.cancel_current();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            ipc::bootstrap,
            ipc::observe_state,
            ipc::submit,
            ipc::finish_capture,
            ipc::cancel_turn,
            ipc::update_settings,
            ipc::set_groq_key,
            ipc::delete_groq_key,
            ipc::load_history,
            ipc::clear_history,
            ipc::diagnostics,
            updates::check_update,
            updates::install_update,
        ])
        .build(tauri::generate_context!());
    let app = match app {
        Ok(app) => app,
        Err(_) => {
            friday_adapters::windows_host::startup_error();
            return;
        }
    };
    app.run(|app, event| {
        if let RunEvent::ExitRequested { api, code, .. } = event {
            let Some(host) = app.try_state::<Host>() else {
                return;
            };
            if !host.closing.swap(true, Ordering::AcqRel) {
                api.prevent_exit();
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    app.state::<Host>().shutdown().await;
                    app.exit(code.unwrap_or(0));
                });
            }
        }
    });
}
