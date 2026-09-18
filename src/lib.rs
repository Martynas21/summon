#[cfg(not(target_os = "macos"))]
compile_error!("summon is macOS-only. Windows support was removed; see git history.");

pub mod app;
pub mod cli;
pub mod config;
pub mod cycle_state;
pub mod daemon;
pub mod dispatch;
pub mod hotkey;
pub mod ipc;
pub mod launchd;
pub mod paths;
pub mod permissions;
pub mod proc;
pub mod screen;
pub mod summoner;
pub mod tui;
pub mod window;
