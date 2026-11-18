use std::{mem::size_of, sync::mpsc, time::Duration};

use friday_engine::{EngineHandle, Failure, SessionStatus};
use windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
        System::{
            LibraryLoader::GetModuleHandleW,
            RemoteDesktop::{
                NOTIFY_FOR_THIS_SESSION, WTS_CURRENT_SESSION, WTS_SESSIONSTATE_LOCK,
                WTS_SESSIONSTATE_UNLOCK, WTSActive, WTSFreeMemory, WTSINFOEXW,
                WTSQuerySessionInformationW, WTSRegisterSessionNotification, WTSSessionInfoEx,
                WTSUnRegisterSessionNotification,
            },
        },
        UI::WindowsAndMessaging::{
            CREATESTRUCTW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
            GWLP_USERDATA, GetMessageW, GetWindowLongPtrW, MSG, PBT_APMRESUMEAUTOMATIC,
            PBT_APMRESUMESUSPEND, PBT_APMSUSPEND, PostMessageW, PostQuitMessage, RegisterClassW,
            SetWindowLongPtrW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_DESTROY, WM_NCCREATE,
            WM_POWERBROADCAST, WM_WTSSESSION_CHANGE, WNDCLASSW, WTS_CONSOLE_CONNECT,
            WTS_CONSOLE_DISCONNECT, WTS_REMOTE_CONNECT, WTS_REMOTE_DISCONNECT, WTS_SESSION_LOCK,
            WTS_SESSION_LOGOFF, WTS_SESSION_UNLOCK,
        },
    },
    core::{PCWSTR, PWSTR, w},
};

pub struct SessionMonitor {
    window: usize,
}

impl SessionMonitor {
    pub fn start(engine: EngineHandle) -> Result<Self, Failure> {
        let (ready, received) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("friday-windows-session".into())
            .spawn(move || monitor(engine, ready))
            .map_err(|_| Failure::Locked)?;
        let window = received
            .recv_timeout(Duration::from_secs(2))
            .map_err(|_| Failure::Locked)?;
        Ok(Self { window })
    }
}

impl Drop for SessionMonitor {
    fn drop(&mut self) {
        // SAFETY: the monitor thread owns the window and handles the posted close message.
        unsafe {
            let _ = PostMessageW(Some(HWND(self.window as _)), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

struct Watcher {
    engine: EngineHandle,
    suspended: bool,
    closed: bool,
}

fn monitor(engine: EngineHandle, ready: mpsc::SyncSender<usize>) {
    let mut state = Watcher {
        engine,
        suspended: false,
        closed: false,
    };
    let name = w!("FridayDesktopSessionMonitor");
    // SAFETY: instances and opaque pointers are used only by this thread's window
    // procedure. The state remains pinned on the stack through DestroyWindow.
    unsafe {
        let Ok(instance) = GetModuleHandleW(PCWSTR::null()) else {
            return;
        };
        let instance = HINSTANCE(instance.0);
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: name,
            ..Default::default()
        };
        if RegisterClassW(&class) == 0 {
            return;
        }
        let Ok(window) = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            name,
            name,
            WINDOW_STYLE::default(),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance),
            Some((&mut state as *mut Watcher).cast()),
        ) else {
            return;
        };
        if WTSRegisterSessionNotification(window, NOTIFY_FOR_THIS_SESSION).is_err() {
            let _ = DestroyWindow(window);
            return;
        }
        state.engine.set_session(current_session());
        if ready.send(window.0 as usize).is_ok() {
            let mut message = MSG::default();
            while GetMessageW(&mut message, None, 0, 0).0 > 0 {
                DispatchMessageW(&message);
            }
        }
        if !state.closed {
            let _ = WTSUnRegisterSessionNotification(window);
            let _ = DestroyWindow(window);
        }
    }
    state.engine.set_session(SessionStatus::Unknown);
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: WM_NCCREATE owns a live CREATESTRUCTW and the registered userdata is a
    // same-thread Watcher for the lifetime of this window.
    unsafe {
        if message == WM_NCCREATE && lparam.0 != 0 {
            let created = &*(lparam.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(window, GWLP_USERDATA, created.lpCreateParams as isize);
        }
        let stored = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Watcher;
        if let Some(state) = stored.as_mut() {
            if message == WM_DESTROY {
                state.closed = true;
            }
            if message == WM_WTSSESSION_CHANGE {
                match wparam.0 as u32 {
                    WTS_SESSION_LOCK
                    | WTS_SESSION_LOGOFF
                    | WTS_CONSOLE_DISCONNECT
                    | WTS_REMOTE_DISCONNECT => {
                        state.engine.set_session(SessionStatus::Locked);
                    }
                    WTS_SESSION_UNLOCK | WTS_CONSOLE_CONNECT | WTS_REMOTE_CONNECT
                        if !state.suspended =>
                    {
                        state.engine.set_session(current_session());
                    }
                    _ => {}
                }
                return LRESULT(0);
            }
            if message == WM_POWERBROADCAST {
                match wparam.0 as u32 {
                    PBT_APMSUSPEND => {
                        state.suspended = true;
                        state.engine.set_session(SessionStatus::Suspended);
                    }
                    PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND => {
                        state.suspended = false;
                        state.engine.set_session(current_session());
                    }
                    _ => {}
                }
                return LRESULT(1);
            }
        }
        match message {
            WM_CLOSE => {
                let _ = WTSUnRegisterSessionNotification(window);
                let _ = DestroyWindow(window);
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(window, message, wparam, lparam),
        }
    }
}

pub(super) fn current_session() -> SessionStatus {
    let mut pointer = PWSTR::null();
    let mut bytes = 0_u32;
    // SAFETY: WTS owns the returned buffer and reports its size. It is copied
    // only when large enough for WTSINFOEXW, then freed with the matching API.
    unsafe {
        if WTSQuerySessionInformationW(
            None,
            WTS_CURRENT_SESSION,
            WTSSessionInfoEx,
            &mut pointer,
            &mut bytes,
        )
        .is_err()
            || pointer.is_null()
        {
            return SessionStatus::Unknown;
        }
        let info = (bytes as usize >= size_of::<WTSINFOEXW>())
            .then(|| (pointer.0 as *const WTSINFOEXW).read_unaligned());
        WTSFreeMemory(pointer.0.cast());
        let Some(info) = info.filter(|info| info.Level == 1) else {
            return SessionStatus::Unknown;
        };
        let data = info.Data.WTSInfoExLevel1;
        if data.SessionState != WTSActive {
            return SessionStatus::Locked;
        }
        match data.SessionFlags as u32 {
            WTS_SESSIONSTATE_UNLOCK => SessionStatus::Unlocked,
            WTS_SESSIONSTATE_LOCK => SessionStatus::Locked,
            _ => SessionStatus::Unknown,
        }
    }
}
