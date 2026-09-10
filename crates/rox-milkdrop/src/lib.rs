//! MilkDrop visuals, rendered by libprojectM on a thread of their own.
//!
//! A MilkDrop preset is a little program: a per-frame equation block, a
//! per-vertex warp mesh, and hand-written GLSL for the composite. Twenty
//! years of them exist, thousands of files, and there's no path to running
//! them that doesn't run their GLSL. So rox doesn't reimplement MilkDrop; it
//! embeds libprojectM, which is the reference implementation, and gives it
//! what it needs: an OpenGL context, a framebuffer, and audio.
//!
//! That context is the reason this crate exists as a crate. rox draws through
//! gpui, which draws through blade on Vulkan and Metal and through D3D11 on
//! Windows, and none of those will let a second renderer scribble into their
//! swapchain. So the engine owns a private, windowless GL context on its own
//! thread ([`context`]), renders into its own FBO, reads the pixels back, and
//! publishes an rgba8 buffer. The panel picks that buffer up and uploads it
//! as a texture. Nothing in here knows gpui exists, and nothing in here draws
//! a pixel of rox's UI.
//!
//! ## The readback, said out loud
//!
//! Reading a frame off the GPU and pushing it back up as a texture is a cost
//! ADR 8 refused for the generative visual, and ADR 28 takes on purpose here,
//! because the alternative is porting MilkDrop's shader language to WGSL. The
//! readback goes through two pixel buffer objects: frame N's `glReadPixels`
//! starts into one while frame N-1's pixels are mapped out of the other. That
//! buys one frame of latency and spends it on not stalling the GL pipeline
//! waiting for a synchronous read, which is the difference between a smooth
//! sixty and a sawtooth thirty.
//!
//! What the panel picks up is a handle, not a copy. [`Engine::frame_after`]
//! used to clone the whole buffer out of the slot, and the texture upload
//! copied it again, which at a 776x1049 panel measured 2.6 ms of UI thread
//! per frame: two three-megabyte allocations faulting in a page at a time,
//! with the memcpys themselves only a fortieth of it. Now the pixels live
//! behind an `Arc` and the worker takes the buffer back once the last handle
//! is gone, so a steady state allocates nothing on either side. The readback
//! itself is still a readback; a zero-copy import (external memory on Linux,
//! D3D interop on Windows) is the planned follow-up, and the numbers this
//! crate's `examples/headless.rs` prints are what it gets judged against.
//!
//! ## Shape
//!
//! [`Engine::spawn`] starts the worker and returns immediately, because
//! context creation can take a second on a cold driver and a panel opening
//! shouldn't wait on it. Everything after that is one-way: [`Command`]s go
//! down the channel, frames and [`Event`]s and [`Status`] come back through
//! shared state. Failure is a [`Status::Failed`] with a sentence in it, never
//! a panic, because a machine with no usable GL is a normal machine that
//! should get every other panel working.

pub mod context;
mod gl;
pub mod library;
pub mod thumbs;
mod worker;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crossbeam_channel::Sender;

pub use library::{PresetLibrary, Rotation};

/// One rendered frame: straight-alpha rgba8, `width * height * 4` bytes, top
/// row first. `glReadPixels` hands back bottom-up rows and the worker flips
/// them, so this is already in the orientation a texture upload wants.
///
/// The pixels are shared, not owned, so handing a frame out is a refcount
/// bump rather than a megabyte-scale copy. The worker takes the buffer back
/// when the last handle to it is gone, which is what keeps a steady state
/// from allocating.
#[derive(Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba8: Arc<Vec<u8>>,
    /// Monotonic per engine. The panel keeps the seq it last drew and passes
    /// it to [`Engine::frame_after`], which is how it skips a frame it has.
    pub seq: u64,
}

/// What the worker is doing, for the panel to show and for a failure to be
/// visible instead of silent.
#[derive(Clone, Debug)]
pub enum Status {
    /// Spawned, context not up yet.
    Starting,
    Running {
        preset: Option<PathBuf>,
        projectm_version: String,
        /// `GL_RENDERER` and `GL_VERSION` as the driver reports them. The
        /// panel names them when the engine runs but no frame arrives, so
        /// a bug report carries the one fact that decides most of them.
        renderer: String,
        gl_version: String,
    },
    /// No usable OpenGL, or projectM refused to start. The string names the
    /// platform and the step, and is meant to be shown to the user.
    Failed(String),
}

/// Things that happened since the panel last looked.
#[derive(Clone, Debug)]
pub enum Event {
    PresetChanged(PathBuf),
    PresetFailed { path: PathBuf, message: String },
}

/// What the panel asks the worker for. Everything is fire and forget: a
/// command that arrives after the worker has failed is dropped, which is the
/// same as it being ignored, which is what a dead engine should do.
pub enum Command {
    /// Render size in device pixels. Reallocates the FBO, its texture, and
    /// both readback buffers, so the panel debounces this during a drag.
    Resize {
        width: u32,
        height: u32,
    },
    LoadPreset {
        path: PathBuf,
        smooth: bool,
    },
    /// Random pick from the rotation, honouring `locked`.
    NextPreset {
        smooth: bool,
    },
    PreviousPreset {
        smooth: bool,
    },
    /// Narrow what `NextPreset`, `PreviousPreset` and projectM's own timed
    /// switch walk. A rotation that selects nothing falls back to the whole
    /// library, so a folder the user deleted since last run doesn't strand
    /// the panel on one preset.
    SetRotation(Rotation),
    /// Replace the library a running worker walks, after a rescan found
    /// presets that weren't there when the panel opened. `rotation` is
    /// re-resolved against the new list, since the old indices mean nothing
    /// once the list changes. A worker that had nothing to show and now does
    /// loads a preset straight away rather than sitting on projectM's idle
    /// one until something else nudges it.
    SetLibrary {
        library: PresetLibrary,
        rotation: Rotation,
    },
    SetLocked(bool),
    SetPresetDuration(f64),
    SetBeatSensitivity(f32),
    SetHardCut(bool),
    SetFps(u32),
    /// Stop rendering, keep the context. A parked panel sends this.
    Pause,
    Resume,
}

pub struct EngineOptions {
    pub feed: Arc<rox_viz::AudioFeed>,
    pub library: PresetLibrary,
    /// The preset to come up on, if the caller is restoring one. With
    /// `None` the worker shuffles one from the library. A restored preset
    /// sent as a command after spawn would land behind that shuffle, and
    /// the owner would see two switches at start: the random one, then
    /// the restore. The backdrop writes the preset it sees to settings
    /// while locked, and with both events in one drain it kept the random
    /// one, so every restart came up somewhere else.
    pub preset: Option<PathBuf>,
    pub fps: u32,
    pub width: u32,
    pub height: u32,
}

/// Everything the worker publishes and the panel reads.
///
/// `seq` is out here as an atomic rather than inside the frame lock so the
/// panel can answer "is there anything new" without contending with the
/// worker mid-publish. It's the only field read on every UI frame.
pub(crate) struct Shared {
    pub(crate) seq: AtomicU64,
    /// The last frame's map-and-flip, in microseconds. Only the headless
    /// example reads it, and it's an atomic so reading it costs nothing.
    pub(crate) readback_micros: AtomicU64,
    pub(crate) frame: Mutex<Option<Frame>>,
    pub(crate) status: Mutex<Status>,
    pub(crate) events: Mutex<Vec<Event>>,
}

impl Shared {
    fn new() -> Shared {
        Shared {
            seq: AtomicU64::new(0),
            readback_micros: AtomicU64::new(0),
            frame: Mutex::new(None),
            status: Mutex::new(Status::Starting),
            events: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn set_status(&self, status: Status) {
        *self.status.lock().unwrap() = status;
    }

    pub(crate) fn push_event(&self, event: Event) {
        let mut events = self.events.lock().unwrap();
        // A panel that stopped draining, because it's parked or the window
        // is hidden, must not grow this without bound.
        if events.len() < 64 {
            events.push(event);
        }
    }
}

/// The worker's mailbox. Cloneable and cheap; the last clone dropped shuts
/// the worker down and waits for it.
#[derive(Clone)]
pub struct Engine {
    inner: Arc<EngineInner>,
}

struct EngineInner {
    /// `Option` only so `Drop` can hang up the channel before joining. It is
    /// `Some` for the whole life of the engine otherwise.
    commands: Option<Sender<Command>>,
    shared: Arc<Shared>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl Engine {
    /// Spawn the worker: GL context, projectM instance, render loop. Returns
    /// as soon as the thread is running, which is before the context exists.
    /// Errors arrive through [`Engine::status`], never as a panic.
    pub fn spawn(options: EngineOptions) -> Engine {
        let shared = Arc::new(Shared::new());
        let (commands, receiver) = crossbeam_channel::unbounded();

        let worker_shared = Arc::clone(&shared);
        let worker = std::thread::Builder::new()
            .name("rox-milkdrop".to_string())
            .spawn(move || worker::run(options, receiver, worker_shared))
            .ok();

        if worker.is_none() {
            shared.set_status(Status::Failed(
                "rox could not start the Milkdrop render thread".to_string(),
            ));
        }

        Engine {
            inner: Arc::new(EngineInner {
                commands: Some(commands),
                shared,
                worker: Mutex::new(worker),
            }),
        }
    }

    pub fn send(&self, command: Command) {
        if let Some(commands) = self.inner.commands.as_ref() {
            // A closed channel means the worker gave up. Nothing to report:
            // `status()` already says why.
            let _ = commands.send(command);
        }
    }

    /// The newest frame if its seq is past `after`. The caller keeps the seq
    /// it last drew and passes it back here.
    ///
    /// What comes back is a handle on the worker's pixels, so this costs a
    /// refcount bump and the lock is held for exactly that long. Hold the
    /// frame no longer than the paint that draws it: the worker can only
    /// reuse the buffer once nothing else is pointing at it, and a consumer
    /// that keeps one around makes the next render allocate.
    pub fn frame_after(&self, after: u64) -> Option<Frame> {
        if self.inner.shared.seq.load(Ordering::Acquire) <= after {
            return None;
        }
        let frame = self.inner.shared.frame.lock().unwrap();
        let frame = frame.as_ref()?;
        if frame.seq <= after {
            return None;
        }
        Some(frame.clone())
    }

    /// Microseconds the worker's last readback spent mapping the pixel
    /// buffer and flipping its rows. This is the cost the zero-copy
    /// follow-up exists to remove, so it's measurable from outside.
    pub fn last_readback_micros(&self) -> u64 {
        self.inner.shared.readback_micros.load(Ordering::Relaxed)
    }

    pub fn status(&self) -> Status {
        self.inner.shared.status.lock().unwrap().clone()
    }

    /// Preset switches and failures since the last call. The panel drains
    /// these each frame to show the preset name and to report broken files.
    pub fn take_events(&self) -> Vec<Event> {
        std::mem::take(&mut *self.inner.shared.events.lock().unwrap())
    }
}

impl Drop for EngineInner {
    fn drop(&mut self) {
        // Hanging up is the shutdown signal: the worker's loop ends when the
        // channel disconnects, and only then does it destroy the projectM
        // instance and drop the context. Joining here means a caller that
        // drops the engine and immediately spawns another doesn't end up
        // with two GL contexts alive at once.
        self.commands = None;
        if let Some(worker) = self.worker.lock().unwrap().take() {
            let _ = worker.join();
        }
    }
}
