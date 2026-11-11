//! macOS window for the Rune editor.
//!
//! baseview owns the child view. A `CAMetalLayer` on that view is the only
//! surface Rune draws. The audio thread wakes the window through a
//! [`WindowWaker`] and never touches the layer or the runtime.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use baseview::dpi::{LogicalSize as WindowLogical, PhysicalPosition, Size as WindowSizeKind};
use baseview::{
    Event, EventStatus, HandlerError, MouseButton, MouseEvent, ScrollDelta, Window, WindowContext,
    WindowHandler, WindowSettings, WindowSize,
};
use keyboard_types::{Key, KeyState, KeyboardEvent, Modifiers, NamedKey};
use nice_plug::context::gui::GuiContext;
use nice_plug::editor::dpi::{self as plug_dpi, LogicalSize, NativeSize};
use nice_plug::editor::{
    Editor, EditorHandle, HostMethods, ParentWindowHandle, ResizeHint, SizeConstraints,
    SpawnedEditor,
};
use nice_plug::plugin::TrackInfo;
use objc2::rc::Retained;
use objc2_app_kit::NSView;
use objc2_quartz_core::{CALayer, CAMetalLayer};
use parking_lot::Mutex;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use rune::embed::LayerHost;
use rune::{Key as RuneKey, KeyEvent as RuneKeyEvent, Mods, Runtime};

use super::face::{self, Model};
use crate::editor::SonomoriEditor;

const ANIMATION_CAP: u32 = 90;

pub struct EditorState {
    open: AtomicBool,
    width: AtomicU32,
    height: AtomicU32,
    scale: AtomicU32,
}

impl EditorState {
    fn new(width: f32, height: f32, scale: f32) -> Self {
        Self {
            open: AtomicBool::new(false),
            width: AtomicU32::new(width.to_bits()),
            height: AtomicU32::new(height.to_bits()),
            scale: AtomicU32::new(scale.to_bits()),
        }
    }

    fn load(&self) -> (f32, f32, f32) {
        (
            f32::from_bits(self.width.load(Ordering::Relaxed)),
            f32::from_bits(self.height.load(Ordering::Relaxed)),
            f32::from_bits(self.scale.load(Ordering::Relaxed)),
        )
    }

    fn store(&self, width: f32, height: f32, scale: f32) {
        self.width.store(width.to_bits(), Ordering::Relaxed);
        self.height.store(height.to_bits(), Ordering::Relaxed);
        self.scale.store(scale.to_bits(), Ordering::Relaxed);
    }
}

struct RepaintInner {
    flag: AtomicBool,
    waker: Mutex<Option<baseview::WindowWaker>>,
}

#[derive(Clone)]
pub struct Repaint {
    inner: Arc<RepaintInner>,
}

impl Repaint {
    fn new() -> Self {
        Self {
            inner: Arc::new(RepaintInner {
                flag: AtomicBool::new(false),
                waker: Mutex::new(None),
            }),
        }
    }
}

pub struct PluginEditor {
    state: Arc<EditorState>,
    editor: Arc<Mutex<SonomoriEditor>>,
    repaint: Repaint,
    title: String,
    resize: ResizeHint,
}

pub fn new_state(size: LogicalSize<f32>) -> Arc<EditorState> {
    Arc::new(EditorState::new(size.width, size.height, 1.0))
}

pub fn new_repaint() -> Repaint {
    Repaint::new()
}

#[inline]
pub fn is_open(state: &EditorState) -> bool {
    state.open.load(Ordering::Acquire)
}

#[inline]
pub fn request_repaint(repaint: &Repaint) {
    repaint.inner.flag.store(true, Ordering::Release);
    if let Some(waker) = repaint.inner.waker.lock().as_ref() {
        waker.request_redraw();
    }
}

pub fn open(
    state: Arc<EditorState>,
    repaint: Repaint,
    title: &str,
    resize: ResizeHint,
    editor: SonomoriEditor,
) -> Option<PluginEditor> {
    rune::embed::prewarm();
    Some(PluginEditor {
        state,
        editor: Arc::new(Mutex::new(editor)),
        repaint,
        title: title.to_owned(),
        resize,
    })
}

impl Editor for PluginEditor {
    type Handle = RuneEditorHandle;

    fn spawn(
        &self,
        parent: Option<ParentWindowHandle>,
        wait_for_parent: bool,
        fallback_scale_factor: Option<f64>,
        gui_context: GuiContext,
        host: Option<HostMethods>,
    ) -> Result<SpawnedEditor<Self::Handle>, Box<dyn std::error::Error>> {
        if self.state.open.load(Ordering::Acquire) {
            return Err(std::io::Error::other("the editor window is already open").into());
        }
        let (width, height, _) = self.state.load();
        let settings = WindowSettings::new()
            .with_title(self.title.clone())
            .with_size(WindowSizeKind::Logical(WindowLogical::new(
                width as f64,
                height as f64,
            )))
            .with_resizable(self.resize.can_resize)
            .with_min_size::<WindowSizeKind>(constraint_size(self.resize.size_constraints, true))
            .with_max_size::<WindowSizeKind>(constraint_size(self.resize.size_constraints, false))
            .with_parent(parent.as_ref())
            .with_fallback_scale_factor(fallback_scale_factor)
            .with_wait_for_parent(wait_for_parent);

        let editor = Arc::clone(&self.editor);
        let repaint = self.repaint.clone();
        let state = Arc::clone(&self.state);
        let window = Window::create_with_host(
            settings,
            move |ctx| RuneWindow::new(ctx, editor, gui_context, repaint, state),
            host_adapter(host),
        )?;
        self.state.open.store(true, Ordering::Release);

        Ok(SpawnedEditor {
            handle: RuneEditorHandle {
                state: Arc::clone(&self.state),
                repaint: self.repaint.clone(),
                resize: self.resize,
            },
            window,
        })
    }

    fn size(&self) -> NativeSize<u32> {
        let (width, height, scale) = self.state.load();
        NativeSize::from_size(
            plug_dpi::Size::Logical(plug_dpi::LogicalSize::new(width as f64, height as f64)),
            scale as f64,
        )
    }

    fn resize_hint(&self) -> ResizeHint {
        self.resize
    }

    fn track_info_updated(&self, _info: TrackInfo) {
        request_repaint(&self.repaint);
    }

    unsafe fn assume_standalone_in_process(&self) {
        unsafe { baseview::assume_standalone_in_process() }
    }
}

pub struct RuneEditorHandle {
    state: Arc<EditorState>,
    repaint: Repaint,
    resize: ResizeHint,
}

impl EditorHandle for RuneEditorHandle {
    type Window = Window;
    type Error = baseview::Error;

    fn run_until_closed(window: Self::Window) -> Result<(), Self::Error> {
        window.run_until_closed()
    }

    fn set_parent(
        &self,
        parent: ParentWindowHandle,
        window: &Self::Window,
    ) -> Result<(), Self::Error> {
        window.set_parent(&parent)
    }

    fn show(&self, window: &Self::Window) -> Result<(), Self::Error> {
        let result = window.show();
        request_repaint(&self.repaint);
        result
    }

    fn hide(&self, window: &Self::Window) -> Result<(), Self::Error> {
        window.hide()
    }

    fn host_main_thread_callback(&self, window: &Self::Window) {
        window.host_main_thread_callback();
    }

    fn set_size(
        &self,
        new_size: NativeSize<u32>,
        window: &Self::Window,
    ) -> Result<(), Self::Error> {
        let (_, _, scale) = self.state.load();
        self.state
            .store(new_size.width as f32, new_size.height as f32, scale);
        window.resize(WindowSizeKind::Logical(WindowLogical::new(
            new_size.width as f64,
            new_size.height as f64,
        )))
    }

    fn set_fallback_scale_factor(
        &self,
        scale_factor: f64,
        window: &Self::Window,
    ) -> Result<(), Self::Error> {
        window.suggest_fallback_scale_factor(scale_factor)
    }

    fn adjust_size(
        &self,
        new_size: NativeSize<u32>,
        window: &Self::Window,
    ) -> Option<NativeSize<u32>> {
        let current = window.size();
        Some(self.resize.adjust_size(
            new_size,
            NativeSize::from_logical_or_physical(current.logical, current.physical),
            current.scale_factor,
        ))
    }

    fn state_changed(&self) {
        request_repaint(&self.repaint);
    }

    fn param_value_changed(&self, _id: &str, _normalized_value: f32) {
        request_repaint(&self.repaint);
    }

    fn param_modulation_changed(&self, _id: &str, _modulation_offset: f32) {
        request_repaint(&self.repaint);
    }
}

impl Drop for RuneEditorHandle {
    fn drop(&mut self) {
        self.state.open.store(false, Ordering::Release);
    }
}

struct Gpu {
    host: LayerHost<Model>,
    layer: Retained<CAMetalLayer>,
}

struct RuneWindow {
    ctx: WindowContext,
    editor: Arc<Mutex<SonomoriEditor>>,
    gui: RefCell<Option<GuiContext>>,
    gpu: RefCell<Option<Gpu>>,
    repaint: Repaint,
    state: Arc<EditorState>,
    cursor: Cell<(f32, f32)>,
    anim_frames: Cell<u32>,
}

impl RuneWindow {
    fn new(
        ctx: WindowContext,
        editor: Arc<Mutex<SonomoriEditor>>,
        gui: GuiContext,
        repaint: Repaint,
        state: Arc<EditorState>,
    ) -> Result<Self, HandlerError> {
        let waker = ctx.waker();
        *repaint.inner.waker.lock() = Some(waker.clone());
        if repaint.inner.flag.swap(false, Ordering::AcqRel) {
            waker.request_redraw();
        }
        Ok(Self {
            ctx,
            editor,
            gui: RefCell::new(Some(gui)),
            gpu: RefCell::new(None),
            repaint,
            state,
            cursor: Cell::new((0.0, 0.0)),
            anim_frames: Cell::new(0),
        })
    }

    fn ensure(&self) -> Result<(), HandlerError> {
        if self.gpu.borrow().is_some() {
            return Ok(());
        }
        let size = self.ctx.size();
        if size.logical.width < 1.0 || size.logical.height < 1.0 {
            self.ctx.request_redraw();
            return Ok(());
        }
        let view = self.ns_view()?;
        let layer = CAMetalLayer::new();
        configure_layer(&layer, &view, size);
        let spec = face::spec();
        if let Some(gui) = self.gui.borrow_mut().take() {
            self.editor.lock().attach_gui(gui);
        }
        let model = Model::new(Arc::clone(&self.editor));
        let wake = {
            let waker = self.ctx.waker();
            Arc::new(move || waker.request_redraw()) as rune::Waker
        };
        // The retained layer outlives the host: `Gpu` drops the host first.
        let host = unsafe {
            LayerHost::from_core_animation_layer(
                Retained::as_ptr(&layer) as *mut c_void,
                &spec,
                model,
                (size.logical.width as f32, size.logical.height as f32),
                size.scale_factor as f32,
                true,
                wake,
            )
        };
        let host = match host {
            Ok(host) => host,
            Err(message) => {
                view.setLayer(None);
                self.editor.lock().detach_gui();
                return Err(fail(&message));
            }
        };
        self.store_size(size);
        *self.gpu.borrow_mut() = Some(Gpu { host, layer });
        Ok(())
    }

    fn ns_view(&self) -> Result<Retained<NSView>, HandlerError> {
        let handle = self
            .ctx
            .window_handle()
            .map_err(|_| fail("the window has no view"))?;
        let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
            return Err(fail("the editor window is not an AppKit view"));
        };
        let ptr = appkit.ns_view.as_ptr().cast::<NSView>();
        unsafe { Retained::retain(ptr) }
            .ok_or_else(|| fail("the AppKit view could not be retained"))
    }

    fn store_size(&self, size: WindowSize) {
        self.state.store(
            size.logical.width as f32,
            size.logical.height as f32,
            size.scale_factor as f32,
        );
    }

    fn sync_layer(&self, size: WindowSize) -> Result<(), HandlerError> {
        if self.gpu.borrow().is_none() {
            return Ok(());
        }
        let view = self.ns_view()?;
        let mut gpu = self.gpu.borrow_mut();
        let Some(gpu) = gpu.as_mut() else {
            return Ok(());
        };
        configure_layer(&gpu.layer, &view, size);
        gpu.host.resize(
            (size.logical.width as f32, size.logical.height as f32),
            size.scale_factor as f32,
        );
        Ok(())
    }

    fn note_mods(&self, mods: Modifiers) {
        let gpu = self.gpu.borrow();
        let Some(gpu) = gpu.as_ref() else {
            return;
        };
        let chrome = &gpu.host.runtime().state.chrome;
        chrome.shift.set(mods.shift());
        chrome.command.set(mods.meta() || mods.ctrl());
    }

    fn with_runtime(&self, body: impl FnOnce(&mut Runtime<Model>)) {
        let mut gpu = self.gpu.borrow_mut();
        let Some(gpu) = gpu.as_mut() else {
            return;
        };
        body(gpu.host.runtime_mut());
    }

    fn to_points(&self, position: PhysicalPosition<f64>) -> (f32, f32) {
        let scale = self.ctx.size().scale_factor.max(1.0);
        ((position.x / scale) as f32, (position.y / scale) as f32)
    }

    fn on_mouse(&self, event: MouseEvent) -> EventStatus {
        if self.gpu.borrow().is_none() {
            self.ctx.request_redraw();
            return EventStatus::Captured;
        }
        match event {
            MouseEvent::CursorMoved {
                position,
                modifiers,
            } => {
                self.note_mods(modifiers);
                let point = self.to_points(position);
                self.cursor.set(point);
                self.with_runtime(|runtime| runtime.pointer_move(point.0, point.1));
            }
            MouseEvent::ButtonPressed {
                button: MouseButton::Left,
                modifiers,
            } => {
                self.note_mods(modifiers);
                let (x, y) = self.cursor.get();
                self.with_runtime(|runtime| runtime.pointer_down(x, y));
            }
            MouseEvent::ButtonReleased {
                button: MouseButton::Left,
                modifiers,
            } => {
                self.note_mods(modifiers);
                let (x, y) = self.cursor.get();
                self.with_runtime(|runtime| runtime.pointer_up(x, y));
            }
            MouseEvent::WheelScrolled { delta, modifiers } => {
                self.note_mods(modifiers);
                let (x, y) = self.cursor.get();
                let scale = self.ctx.size().scale_factor.max(1.0) as f32;
                let dy = match delta {
                    ScrollDelta::Lines { y, .. } => y * 16.0,
                    ScrollDelta::Pixels { y, .. } => y / scale,
                };
                self.with_runtime(|runtime| runtime.wheel(x, y, dy));
            }
            _ => {}
        }
        self.ctx.request_redraw();
        EventStatus::Captured
    }

    fn on_key(&self, event: KeyboardEvent) -> EventStatus {
        self.note_mods(event.modifiers);
        let focused = self
            .gpu
            .borrow()
            .as_ref()
            .is_some_and(|gpu| gpu.host.runtime().has_focus());
        if !focused {
            return EventStatus::Ignored;
        }
        if event.is_composing || is_modifier(&event.key) {
            return EventStatus::Captured;
        }
        let Some(key) = map_key(&event.key, event.modifiers) else {
            return EventStatus::Captured;
        };
        let rune_event = RuneKeyEvent {
            key,
            mods: map_mods(event.modifiers),
        };
        self.with_runtime(|runtime| match event.state {
            KeyState::Down if event.repeat => runtime.repeat_key(rune_event),
            KeyState::Down => runtime.key(rune_event),
            KeyState::Up => {
                runtime.key_up(rune_event);
            }
        });
        self.ctx.request_redraw();
        EventStatus::Captured
    }

    fn after_frame(&self, wants: bool, animating: bool, wake_after: Option<f64>) {
        if wants {
            self.anim_frames.set(0);
            self.ctx.request_redraw();
            return;
        }
        // A spring that never settles must not spin the UI thread.
        if animating {
            let frames = self.anim_frames.get().saturating_add(1);
            self.anim_frames.set(frames);
            if frames < ANIMATION_CAP {
                self.ctx.request_redraw();
                return;
            }
        } else {
            self.anim_frames.set(0);
        }
        if let Some(secs) = wake_after {
            self.ctx
                .request_redraw_after(Duration::from_secs_f64(secs.max(0.0)));
        }
    }
}

impl WindowHandler for RuneWindow {
    fn draw(&self) -> Result<(), HandlerError> {
        self.ensure()?;
        // The layer borrow ends before the redraw request, so a wake cannot
        // re-enter the runtime.
        let painted = {
            let mut gpu = self.gpu.borrow_mut();
            let Some(gpu) = gpu.as_mut() else {
                return Ok(());
            };
            let frame = gpu.host.frame();
            (
                gpu.host.runtime().state.chrome.wants_frame.get(),
                frame.animating,
                frame.wake_after_secs,
            )
        };
        self.after_frame(painted.0, painted.1, painted.2);
        Ok(())
    }

    fn resized(&self, new_size: WindowSize) -> Result<(), HandlerError> {
        self.store_size(new_size);
        self.sync_layer(new_size)
    }

    fn on_event(&self, event: Event) -> EventStatus {
        match event {
            Event::Mouse(mouse) => self.on_mouse(mouse),
            Event::Keyboard(key) => self.on_key(key),
            Event::Window(_) => EventStatus::Ignored,
            _ => EventStatus::Ignored,
        }
    }
}

impl Drop for RuneWindow {
    fn drop(&mut self) {
        self.gpu.borrow_mut().take();
        self.editor.lock().detach_gui();
        self.state.open.store(false, Ordering::Release);
        *self.repaint.inner.waker.lock() = None;
    }
}

fn configure_layer(layer: &Retained<CAMetalLayer>, view: &NSView, size: WindowSize) {
    // `CAMetalLayer` is a `CALayer`. The cast keeps the layer methods without
    // relying on a deref that objc2 does not provide.
    let layer_ptr = Retained::as_ptr(layer) as *const CALayer;
    let as_layer = unsafe { &*layer_ptr };
    as_layer.setContentsScale(size.scale_factor);
    as_layer.setOpaque(true);
    as_layer.setFrame(view.bounds());
    view.setWantsLayer(true);
    view.setLayer(Some(as_layer));
}

fn constraint_size(constraints: SizeConstraints, minimum: bool) -> Option<WindowSizeKind> {
    let logical = match constraints {
        SizeConstraints::Logical { min_size, max_size } => {
            if minimum {
                min_size
            } else {
                max_size
            }
        }
        SizeConstraints::Physical { .. } => return None,
    }?;
    Some(WindowSizeKind::Logical(WindowLogical::new(
        logical.width as f64,
        logical.height as f64,
    )))
}

fn host_adapter(host: Option<HostMethods>) -> Option<baseview::host::Host> {
    let host = host?;
    struct HostCallbackAdapter {
        host: Box<dyn nice_plug::editor::HostCallbacks>,
    }
    impl baseview::host::HostCallbacks for HostCallbackAdapter {
        fn request_resize(&mut self, new_size: WindowSize) -> Result<(), HandlerError> {
            self.host
                .request_resize(new_size.physical.into(), new_size.scale_factor)
                .map_err(HandlerError::from_boxed)
        }

        fn destroyed(&mut self) {
            self.host.destroyed();
        }
    }
    struct HostMainThreadCallerAdapter {
        host: Box<dyn nice_plug::editor::HostMainThreadCaller>,
    }
    impl baseview::host::HostMainThreadCaller for HostMainThreadCallerAdapter {
        fn call_main_thread(&mut self) {
            self.host.call_main_thread();
        }
    }
    Some(
        baseview::host::Host::new()
            .with_callbacks(HostCallbackAdapter {
                host: host.callbacks,
            })
            .with_main_thread(HostMainThreadCallerAdapter {
                host: host.main_thread_caller,
            }),
    )
}

fn fail(message: &str) -> HandlerError {
    std::io::Error::other(message.to_owned()).into()
}

fn is_modifier(key: &Key) -> bool {
    matches!(
        key,
        Key::Named(
            NamedKey::Shift
                | NamedKey::Control
                | NamedKey::Alt
                | NamedKey::Meta
                | NamedKey::Fn
                | NamedKey::AltGraph
        )
    )
}

fn map_key(key: &Key, mods: Modifiers) -> Option<RuneKey> {
    match key {
        Key::Character(text) => {
            let mut chars = text.chars();
            let ch = chars.next()?;
            if chars.next().is_none() && (mods.meta() || mods.ctrl()) {
                Some(RuneKey::Command(ch.to_ascii_lowercase()))
            } else {
                Some(RuneKey::Text(text.clone()))
            }
        }
        Key::Named(named) => match named {
            NamedKey::Enter => Some(RuneKey::Enter),
            NamedKey::Backspace => Some(RuneKey::Backspace),
            NamedKey::Delete => Some(RuneKey::Delete),
            NamedKey::ArrowLeft => Some(RuneKey::Left),
            NamedKey::ArrowRight => Some(RuneKey::Right),
            NamedKey::ArrowUp => Some(RuneKey::Up),
            NamedKey::ArrowDown => Some(RuneKey::Down),
            NamedKey::Home => Some(RuneKey::Home),
            NamedKey::End => Some(RuneKey::End),
            NamedKey::PageUp => Some(RuneKey::PageUp),
            NamedKey::PageDown => Some(RuneKey::PageDown),
            NamedKey::Escape => Some(RuneKey::Escape),
            NamedKey::Tab => Some(RuneKey::Tab),
            _ => None,
        },
    }
}

fn map_mods(mods: Modifiers) -> Mods {
    Mods {
        shift: mods.shift(),
        cmd: mods.meta(),
        alt: mods.alt(),
        ctrl: mods.ctrl(),
        logo: mods.meta(),
    }
}
