//! OS integration surfaces: the MPRIS/media-key controls, the control
//! socket, the icecast broadcast sink's wiring, the system tray for
//! windowless residency, and the taskbar button's progress bar.

pub mod broadcast;
pub mod drive;
pub mod ipc;
pub mod media_controls;
pub mod taskbar;
pub mod tray;
