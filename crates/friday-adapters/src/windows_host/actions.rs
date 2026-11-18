use std::{ffi::OsStr, os::windows::ffi::OsStrExt, path::PathBuf, sync::Arc};

use friday_core::{Action, ActionOutcome, KnownApp, OutputVolume, VolumeChange};
use friday_engine::{ActionPermit, Failure, PlatformActions, PortFuture};
use tokio::sync::Mutex;
use windows::{
    Win32::{
        Media::Audio::{
            Endpoints::IAudioEndpointVolume, IMMDeviceEnumerator, MMDeviceEnumerator, eMultimedia,
            eRender,
        },
        System::{
            Com::{
                CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
            },
            SystemInformation::GetSystemDirectoryW,
        },
        UI::{
            Shell::{ASSOCF_IS_PROTOCOL, ASSOCSTR_EXECUTABLE, AssocQueryStringW, ShellExecuteW},
            WindowsAndMessaging::SW_SHOWNORMAL,
        },
    },
    core::{PCWSTR, PWSTR, w},
};

#[derive(Default)]
pub struct WindowsActions {
    serial: Arc<Mutex<()>>,
}

impl PlatformActions for WindowsActions {
    fn execute(&self, action: Action, permit: ActionPermit) -> PortFuture<'_, ActionOutcome> {
        Box::pin(async move {
            let exclusive = self.serial.clone().lock_owned().await;
            permit.check()?;
            tokio::task::spawn_blocking(move || {
                // This guard outlives a cancelled waiter if a native call is still running.
                let _exclusive = exclusive;
                permit.check()?;
                match action {
                    Action::OpenApp { app } => launch(app, &permit),
                    Action::OutputVolume { change } => volume(change, &permit),
                }
            })
            .await
            .map_err(|_| Failure::ActionUncertain)?
        })
    }
}

fn launch(app: KnownApp, permit: &ActionPermit) -> Result<ActionOutcome, Failure> {
    let path = match app {
        KnownApp::Browser => browser_executable(),
        KnownApp::Calculator => system_calculator(),
    };
    let Some(path) = path else {
        return Ok(ActionOutcome::Failed);
    };
    permit.check()?;
    if super::session::current_session() != friday_engine::SessionStatus::Unlocked {
        return Err(Failure::Locked);
    }
    // SAFETY: path is a live NUL-terminated copy of a system-resolved executable path.
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(path.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    Ok(if result.0 as isize > 32 {
        ActionOutcome::AcceptedByOs
    } else {
        ActionOutcome::Failed
    })
}

fn browser_executable() -> Option<Vec<u16>> {
    let mut path = vec![0_u16; 32_768];
    let mut size = u32::try_from(path.len()).ok()?;
    // SAFETY: the output buffer has exactly the number of writable UTF-16 slots supplied.
    let result = unsafe {
        AssocQueryStringW(
            ASSOCF_IS_PROTOCOL,
            ASSOCSTR_EXECUTABLE,
            w!("https"),
            w!("open"),
            Some(PWSTR(path.as_mut_ptr())),
            &mut size,
        )
    };
    if result.is_err() {
        return None;
    }
    let end = path.iter().position(|unit| *unit == 0)?;
    let name = String::from_utf16(&path[..end]).ok()?;
    let executable = std::path::Path::new(&name);
    if !executable.is_absolute()
        || !executable
            .extension()
            .and_then(OsStr::to_str)
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
    {
        return None;
    }
    path.truncate(end + 1);
    Some(path)
}

fn system_calculator() -> Option<Vec<u16>> {
    let mut path = vec![0_u16; 32_768];
    // SAFETY: the supplied slice describes the entire writable output buffer.
    let size = unsafe { GetSystemDirectoryW(Some(&mut path)) } as usize;
    if size == 0 || size >= path.len() {
        return None;
    }
    let directory = String::from_utf16(&path[..size]).ok()?;
    let path = PathBuf::from(directory).join("calc.exe");
    Some(
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect(),
    )
}

struct ComApartment;

impl ComApartment {
    fn init() -> Result<Self, Failure> {
        // SAFETY: the apartment is initialized on this blocking worker and balanced by Drop.
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
            .ok()
            .map_err(|_| Failure::ActionFailed)?;
        Ok(Self)
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        // SAFETY: this object remains on the thread on which initialization succeeded.
        unsafe { CoUninitialize() };
    }
}

fn volume(change: VolumeChange, permit: &ActionPermit) -> Result<ActionOutcome, Failure> {
    let _apartment = ComApartment::init()?;
    // SAFETY: COM is initialized, all interfaces stay on this worker and the null
    // event-context pointer means no callback correlation is requested.
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(|_| Failure::ActionFailed)?;
        let device = enumerator
            .GetDefaultAudioEndpoint(eRender, eMultimedia)
            .map_err(|_| Failure::ActionFailed)?;
        let endpoint: IAudioEndpointVolume = device
            .Activate(CLSCTX_ALL, None)
            .map_err(|_| Failure::ActionFailed)?;
        let level = scalar_volume(
            endpoint
                .GetMasterVolumeLevelScalar()
                .map_err(|_| Failure::ActionFailed)?,
        )?;
        permit.check()?;
        if super::session::current_session() != friday_engine::SessionStatus::Unlocked {
            return Err(Failure::Locked);
        }
        let written = match change.level_after(level) {
            Some(next) => endpoint
                .SetMasterVolumeLevelScalar(f32::from(next.percent()) / 100.0, std::ptr::null()),
            None => endpoint.SetMute(matches!(change, VolumeChange::Mute), std::ptr::null()),
        };
        if written.is_err() {
            return Ok(ActionOutcome::Failed);
        }
        let observed = endpoint
            .GetMasterVolumeLevelScalar()
            .ok()
            .and_then(|value| scalar_volume(value).ok());
        let muted = endpoint.GetMute().ok();
        Ok(match (observed, muted) {
            (Some(volume), Some(muted)) => ActionOutcome::VolumeApplied {
                volume,
                muted: muted.as_bool(),
            },
            _ => ActionOutcome::Indeterminate,
        })
    }
}

fn scalar_volume(value: f32) -> Result<OutputVolume, Failure> {
    if !value.is_finite() {
        return Err(Failure::ActionFailed);
    }
    OutputVolume::try_from((value.clamp(0.0, 1.0) * 100.0).round() as u8)
        .map_err(|_| Failure::ActionFailed)
}
