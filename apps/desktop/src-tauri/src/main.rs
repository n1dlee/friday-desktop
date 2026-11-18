#![cfg_attr(windows, windows_subsystem = "windows")]
#![forbid(unsafe_code)]

#[cfg(windows)]
mod diagnostics;
#[cfg(windows)]
mod host;
#[cfg(windows)]
mod ipc;
#[cfg(windows)]
mod updates;

#[cfg(windows)]
fn main() {
    host::run();
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Friday Desktop currently supports Windows 11 x64.");
}
