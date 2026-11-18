mod actions;
mod audio;
mod credentials;
mod session;
mod speech;

pub use actions::WindowsActions;
pub use audio::WindowsAudio;
pub use credentials::WindowsCredentials;
pub use session::SessionMonitor;
pub use speech::WindowsSpeech;

pub fn startup_error() {
    use windows::{
        Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW},
        core::w,
    };
    // SAFETY: the message and caption are static NUL-terminated strings.
    unsafe {
        MessageBoxW(
            None,
            w!(
                "Не удалось запустить Friday Desktop. Проверьте доступ к локальному профилю.\nCould not start Friday Desktop. Check access to the local profile."
            ),
            w!("Friday Desktop"),
            MB_ICONERROR | MB_OK,
        );
    }
}
