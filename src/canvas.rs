use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::f64::consts::{PI, TAU};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use cairo::{Context, FontSlant, FontWeight, Format, ImageSurface, Path};
use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;

use crate::model::{
    hex_to_cairo, Note, NoteKind, Page, Point, Rect, Stroke, StrokeTool, A4_RATIO, INFINITE_GRID,
    PAGE_WIDTH,
};
use crate::pdf::PdfDoc;

static NEXT_STROKE_ID: AtomicU64 = AtomicU64::new(1);

fn next_stroke_id() -> u64 {
    NEXT_STROKE_ID.fetch_add(1, Ordering::Relaxed)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Pen,
    Highlighter,
    Eraser,
    Hand,
}

#[derive(Clone, Debug)]
pub struct ViewInfo {
    pub zoom: f64,
    pub page: usize,
    pub page_count: usize,
    pub kind: Option<NoteKind>,
    pub can_undo: bool,
    pub can_redo: bool,
}

#[derive(Clone, Copy, Debug)]
struct Camera {
    tx: f64,
    ty: f64,
    zoom: f64,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            tx: 0.0,
            ty: 0.0,
            zoom: 1.0,
        }
    }
}

enum Interaction {
    None,
    Stroke {
        page: usize,
        stroke: Stroke,
    },
    Erase {
        page: usize,
        hit: Vec<usize>,
    },
    Pan {
        start: (f64, f64),
        camera: Camera,
    },
    Pinch {
        start_center: (f64, f64),
        start_zoom: f64,
        start_camera: Camera,
    },
}

enum LiveItem {
    Stroke(Stroke),
    Erase(Vec<usize>),
}

enum Action {
    AddStroke {
        page: usize,
        index: usize,
        stroke: Stroke,
    },
    Erase {
        page: usize,
        items: Vec<(usize, Stroke)>,
    },
    AddPage {
        index: usize,
    },
}

struct CachedPdf {
    surface: ImageSurface,
}

struct State {
    note: Option<Note>,
    pdf: Option<PdfDoc>,
    layout: Vec<Rect>,
    camera: Camera,
    width: i32,
    height: i32,
    needs_fit: bool,

    tool: Tool,
    color: String,
    pen_size: f64,
    highlighter_size: f64,
    eraser_size: f64,

    interaction: Interaction,
    cursor: Option<(f64, f64)>,
    space_down: bool,
    pinching: bool,
    touch_suppressed: bool,

    undo: Vec<Action>,
    redo: Vec<Action>,

    cache: Option<ImageSurface>,
    cache_dirty: bool,
    path_cache: HashMap<(usize, usize), Path>,
    _scratch_surface: ImageSurface,
    scratch_ctx: Context,

    pdf_pages: HashMap<usize, CachedPdf>,
    pdf_scale: f64,

    on_changed: Option<Rc<dyn Fn()>>,
    on_view: Option<Rc<dyn Fn(ViewInfo)>>,
}

impl State {
    fn new() -> Self {
        let scratch_surface =
            ImageSurface::create(Format::ARgb32, 4, 4).expect("scratch surface");
        let scratch_ctx = Context::new(&scratch_surface).expect("scratch context");
        Self {
            note: None,
            pdf: None,
            layout: Vec::new(),
            camera: Camera::default(),
            width: 0,
            height: 0,
            needs_fit: true,
            tool: Tool::Pen,
            color: "#23262b".into(),
            pen_size: 3.2,
            highlighter_size: 20.0,
            eraser_size: 26.0,
            interaction: Interaction::None,
            cursor: None,
            space_down: false,
            pinching: false,
            touch_suppressed: false,
            undo: Vec::new(),
            redo: Vec::new(),
            cache: None,
            cache_dirty: true,
            path_cache: HashMap::new(),
            _scratch_surface: scratch_surface,
            scratch_ctx,
            pdf_pages: HashMap::new(),
            pdf_scale: 0.0,
            on_changed: None,
            on_view: None,
        }
    }

    #[allow(dead_code)]
    fn active_size(&self) -> f64 {
        match self.tool {
            Tool::Pen => self.pen_size,
            Tool::Highlighter => self.highlighter_size,
            Tool::Eraser => self.eraser_size,
            Tool::Hand => self.pen_size,
        }
    }

    fn screen_to_world(&self, x: f64, y: f64) -> (f64, f64) {
        (
            (x - self.camera.tx) / self.camera.zoom,
            (y - self.camera.ty) / self.camera.zoom,
        )
    }

    fn view_info(&self) -> ViewInfo {
        let page_count = self.note.as_ref().map(|n| n.pages.len()).unwrap_or(0);
        let kind = self.note.as_ref().map(|n| n.kind);
        ViewInfo {
            zoom: self.camera.zoom,
            page: self.current_page(),
            page_count,
            kind,
            can_undo: !self.undo.is_empty(),
            can_redo: !self.redo.is_empty(),
        }
    }

    fn current_page(&self) -> usize {
        let Some(note) = &self.note else {
            return 0;
        };
        if note.kind == NoteKind::Infinite || self.layout.is_empty() {
            return 0;
        }
        let cy = (self.height as f64 / 2.0 - self.camera.ty) / self.camera.zoom;
        if let Some(i) = self
            .layout
            .iter()
            .position(|r| cy >= r.y && cy <= r.y + r.h)
        {
            return i;
        }
        self.layout
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                let da = (a.center().1 - cy).abs();
                let db = (b.center().1 - cy).abs();
                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(i, _)| i)
            .unwrap_or(0)
    }

    fn fit_camera(&mut self) {
        let w = self.width as f64;
        let h = self.height as f64;
        if w < 2.0 || h < 2.0 {
            return;
        }
        let Some(note) = &self.note else {
            self.camera = Camera::default();
            return;
        };
        if note.kind == NoteKind::Infinite {
            self.camera = Camera {
                tx: w * 0.5,
                ty: h * 0.5,
                zoom: 1.0,
            };
            return;
        }
        let Some(rect) = self.layout.first().copied() else {
            return;
        };
        let pad = 44.0;
        let zoom = ((w - pad * 2.0) / rect.w)
            .min((h - pad * 2.0) / rect.h)
            .clamp(0.12, 1.35);
        let tx = (w - rect.w * zoom) * 0.5 - rect.x * zoom;
        let ty = if self.layout.len() == 1 {
            (h - rect.h * zoom) * 0.5
        } else {
            pad
        } - rect.y * zoom;
        self.camera = Camera { tx, ty, zoom };
    }

    fn go_to_page(&mut self, index: usize) {
        let Some(rect) = self.layout.get(index).copied() else {
            return;
        };
        let w = self.width as f64;
        let h = self.height as f64;
        let zoom = self.camera.zoom;
        self.camera.tx = (w - rect.w * zoom) * 0.5 - rect.x * zoom;
        self.camera.ty = (h - rect.h * zoom) * 0.5 - rect.y * zoom;
        self.cache_dirty = true;
    }

    fn zoom_at(&mut self, new_zoom: f64, sx: f64, sy: f64) {
        let new_zoom = new_zoom.clamp(0.15, 4.0);
        let (wx, wy) = self.screen_to_world(sx, sy);
        self.camera.zoom = new_zoom;
        self.camera.tx = sx - wx * new_zoom;
        self.camera.ty = sy - wy * new_zoom;
        self.cache_dirty = true;
    }

    fn invalidate_ink(&mut self) {
        self.path_cache.clear();
        self.cache_dirty = true;
    }
}

pub struct Canvas {
    pub area: gtk::DrawingArea,
    state: Rc<RefCell<State>>,
}

impl Canvas {
    pub fn new() -> Self {
        let area = gtk::DrawingArea::new();
        area.set_hexpand(true);
        area.set_vexpand(true);
        area.set_focusable(true);
        area.add_css_class("sumi-canvas");
        // Invisible cursor: the canvas draws its own tool ring.
        let blank = glib::Bytes::from_static(&[0, 0, 0, 0]);
        let texture = gdk::MemoryTexture::new(
            1,
            1,
            gdk::MemoryFormat::R8g8b8a8Premultiplied,
            &blank,
            4,
        );
        let cursor = gdk::Cursor::from_texture(&texture, 0, 0, None);
        area.set_cursor(Some(&cursor));
        let state = Rc::new(RefCell::new(State::new()));

        area.set_draw_func({
            let state = state.clone();
            move |area, ctx, w, h| {
                draw(area, &state, ctx, w, h);
            }
        });

        // Hover / cursor position.
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion({
            let state = state.clone();
            let area_w = area.downgrade();
            move |controller, x, y| {
                let is_touch = controller
                    .current_event_device()
                    .map(|d| d.source() == gdk::InputSource::Touchscreen)
                    .unwrap_or(false);
                if let Some(area) = area_w.upgrade() {
                    let mut s = state.borrow_mut();
                    s.cursor = if is_touch { None } else { Some((x, y)) };
                    drop(s);
                    if !is_touch {
                        area.queue_draw();
                    }
                }
            }
        });
        motion.connect_leave({
            let state = state.clone();
            let area_w = area.downgrade();
            move |_| {
                if let Some(area) = area_w.upgrade() {
                    state.borrow_mut().cursor = None;
                    area.queue_draw();
                }
            }
        });
        area.add_controller(motion);

        // Stylus / pen with pressure.
        let stylus = gtk::GestureStylus::new();
        stylus.set_stylus_only(true);
        stylus.set_button(1);
        stylus.connect_down({
            let state = state.clone();
            let area_w = area.downgrade();
            move |g, x, y| {
                let pressure = g
                    .axis(gdk::AxisUse::Pressure)
                    .unwrap_or(0.5)
                    .clamp(0.05, 1.0);
                if let Some(area) = area_w.upgrade() {
                    area.grab_focus();
                    let mut s = state.borrow_mut();
                    s.cursor = Some((x, y));
                    pointer_down(&mut s, x, y, pressure);
                    drop(s);
                    area.queue_draw();
                }
            }
        });
        stylus.connect_motion({
            let state = state.clone();
            let area_w = area.downgrade();
            move |g, x, y| {
                let pressure = g
                    .axis(gdk::AxisUse::Pressure)
                    .unwrap_or(0.5)
                    .clamp(0.05, 1.0);
                if let Some(area) = area_w.upgrade() {
                    {
                        let mut s = state.borrow_mut();
                        s.cursor = Some((x, y));
                        pointer_move(&mut s, x, y, pressure);
                    }
                    area.queue_draw();
                }
            }
        });
        stylus.connect_up({
            let state = state.clone();
            let area_w = area.downgrade();
            move |g, x, y| {
                let pressure = g
                    .axis(gdk::AxisUse::Pressure)
                    .unwrap_or(0.5)
                    .clamp(0.05, 1.0);
                if let Some(area) = area_w.upgrade() {
                    let changed = {
                        let mut s = state.borrow_mut();
                        pointer_up(&mut s, x, y, pressure)
                    };
                    area.queue_draw();
                    if changed {
                        notify_changed(&state);
                        emit_view(&state);
                    }
                }
            }
        });
        area.add_controller(stylus);

        // Primary drag: mouse drawing / touch panning.
        let drag = gtk::GestureDrag::new();
        drag.set_button(1);
        let drag_origin = Rc::new(Cell::new((0.0_f64, 0.0_f64)));
        drag.connect_drag_begin({
            let state = state.clone();
            let area_w = area.downgrade();
            let drag_origin = drag_origin.clone();
            move |g, x, y| {
                drag_origin.set((x, y));
                let source = g.current_event_device().map(|d| d.source());
                if let Some(area) = area_w.upgrade() {
                    area.grab_focus();
                    let mut s = state.borrow_mut();
                    match source {
                        Some(gdk::InputSource::Touchscreen) => {
                            if s.pinching || s.touch_suppressed {
                                return;
                            }
                            if matches!(&s.interaction, Interaction::None) {
                                s.interaction = Interaction::Pan {
                                    start: (x, y),
                                    camera: s.camera,
                                };
                            }
                        }
                        Some(gdk::InputSource::Pen) | Some(gdk::InputSource::TabletPad) => {
                            // The stylus gesture handles these when available.
                        }
                        _ => {
                            s.cursor = Some((x, y));
                            pointer_down(&mut s, x, y, 0.5);
                        }
                    }
                    drop(s);
                    area.queue_draw();
                }
            }
        });
        drag.connect_drag_update({
            let state = state.clone();
            let area_w = area.downgrade();
            let drag_origin = drag_origin.clone();
            move |_g, ox, oy| {
                if let Some(area) = area_w.upgrade() {
                    let start = drag_origin.get();
                    let x = start.0 + ox;
                    let y = start.1 + oy;
                    {
                        let mut s = state.borrow_mut();
                        if s.touch_suppressed || s.pinching {
                            return;
                        }
                        if matches!(&s.interaction, Interaction::None) {
                            return;
                        }
                        s.cursor = Some((x, y));
                        pointer_move(&mut s, x, y, 0.5);
                    }
                    area.queue_draw();
                }
            }
        });
        drag.connect_drag_end({
            let state = state.clone();
            let area_w = area.downgrade();
            let drag_origin = drag_origin.clone();
            move |_g, ox, oy| {
                if let Some(area) = area_w.upgrade() {
                    let start = drag_origin.get();
                    let (x, y) = (start.0 + ox, start.1 + oy);
                    let changed = {
                        let mut s = state.borrow_mut();
                        if matches!(&s.interaction, Interaction::None) {
                            s.touch_suppressed = false;
                            return;
                        }
                        s.touch_suppressed = false;
                        pointer_up(&mut s, x, y, 0.5)
                    };
                    area.queue_draw();
                    if changed {
                        notify_changed(&state);
                        emit_view(&state);
                    }
                }
            }
        });
        area.add_controller(drag);

        // Middle-button drag pans.
        let middle = gtk::GestureDrag::new();
        middle.set_button(2);
        middle.connect_drag_begin({
            let state = state.clone();
            let area_w = area.downgrade();
            move |_g, x, y| {
                if let Some(area) = area_w.upgrade() {
                    let mut s = state.borrow_mut();
                    if matches!(&s.interaction, Interaction::None | Interaction::Pan { .. }) {
                        s.interaction = Interaction::Pan {
                            start: (x, y),
                            camera: s.camera,
                        };
                    }
                    drop(s);
                    area.queue_draw();
                }
            }
        });
        middle.connect_drag_update({
            let state = state.clone();
            let area_w = area.downgrade();
            move |_g, ox, oy| {
                if let Some(area) = area_w.upgrade() {
                    {
                        let mut s = state.borrow_mut();
                        let camera = if let Interaction::Pan { camera, .. } = &s.interaction {
                            Some(*camera)
                        } else {
                            None
                        };
                        if let Some(camera) = camera {
                            s.camera.tx = camera.tx + ox;
                            s.camera.ty = camera.ty + oy;
                            s.cache_dirty = true;
                        }
                    }
                    area.queue_draw();
                    emit_view(&state);
                }
            }
        });
        middle.connect_drag_end({
            let state = state.clone();
            let area_w = area.downgrade();
            move |_g, _, _| {
                if let Some(area) = area_w.upgrade() {
                    let mut s = state.borrow_mut();
                    if matches!(&s.interaction, Interaction::Pan { .. }) {
                        s.interaction = Interaction::None;
                    }
                    drop(s);
                    area.queue_draw();
                }
            }
        });
        area.add_controller(middle);

        // Two-finger pinch zoom (and two-finger pan using the gesture center).
        let zoom = gtk::GestureZoom::new();
        zoom.set_propagation_phase(gtk::PropagationPhase::Capture);
        zoom.connect_begin({
            let state = state.clone();
            move |g, _seq| {
                let center = g.bounding_box_center().unwrap_or((0.0, 0.0));
                let mut s = state.borrow_mut();
                if matches!(&s.interaction, Interaction::Stroke { .. } | Interaction::Erase { .. }) {
                    return;
                }
                s.pinching = true;
                s.touch_suppressed = true;
                s.interaction = Interaction::Pinch {
                    start_center: center,
                    start_zoom: s.camera.zoom,
                    start_camera: s.camera,
                };
            }
        });
        zoom.connect_scale_changed({
            let state = state.clone();
            let area_w = area.downgrade();
            move |g, scale| {
                let center = g
                    .bounding_box_center()
                    .unwrap_or((0.0, 0.0));
                if let Some(area) = area_w.upgrade() {
                    {
                        let mut s = state.borrow_mut();
                        if let Interaction::Pinch {
                            start_center,
                            start_zoom,
                            start_camera,
                        } = &s.interaction
                        {
                            let (start_center, start_zoom, start_camera) =
                                (*start_center, *start_zoom, *start_camera);
                            let new_zoom = (start_zoom * scale).clamp(0.15, 4.0);
                            let wx = (start_center.0 - start_camera.tx) / start_camera.zoom;
                            let wy = (start_center.1 - start_camera.ty) / start_camera.zoom;
                            s.camera.zoom = new_zoom;
                            s.camera.tx = center.0 - wx * new_zoom;
                            s.camera.ty = center.1 - wy * new_zoom;
                            s.cache_dirty = true;
                        }
                    }
                    area.queue_draw();
                    emit_view(&state);
                }
            }
        });
        zoom.connect_end({
            let state = state.clone();
            let area_w = area.downgrade();
            move |_g, _seq| {
                if let Some(area) = area_w.upgrade() {
                    {
                        let mut s = state.borrow_mut();
                        s.pinching = false;
                        if matches!(&s.interaction, Interaction::Pinch { .. }) {
                            s.interaction = Interaction::None;
                        }
                    }
                    area.queue_draw();
                    emit_view(&state);
                }
            }
        });
        area.add_controller(zoom);

        // Trackpad / wheel.
        let scroll = gtk::EventControllerScroll::new(
            gtk::EventControllerScrollFlags::BOTH_AXES,
        );
        scroll.connect_scroll({
            let state = state.clone();
            let area_w = area.downgrade();
            move |controller, dx, dy| {
                let mods = controller.current_event_state();
                if let Some(area) = area_w.upgrade() {
                    let (sx, sy) = {
                        let s = state.borrow();
                        s.cursor
                            .unwrap_or((s.width as f64 / 2.0, s.height as f64 / 2.0))
                    };
                    let zooming = mods.intersects(
                        gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::META_MASK,
                    );
                    {
                        let mut s = state.borrow_mut();
                        if zooming {
                            let factor = (-dy * 0.12).exp();
                            let z = s.camera.zoom * factor;
                            s.zoom_at(z, sx, sy);
                        } else {
                            let k = if dy.abs() < 4.0 { 26.0 } else { 1.0 };
                            s.camera.tx -= dx * k;
                            s.camera.ty -= dy * k;
                            s.cache_dirty = true;
                        }
                    }
                    area.queue_draw();
                    emit_view(&state);
                }
                glib::Propagation::Stop
            }
        });
        area.add_controller(scroll);

        // Refresh cache when the system theme changes.
        let style = libadwaita::StyleManager::default();
        style.connect_dark_notify({
            let state = state.clone();
            let area_w = area.downgrade();
            move |_| {
                if let Some(area) = area_w.upgrade() {
                    state.borrow_mut().cache_dirty = true;
                    area.queue_draw();
                }
            }
        });

        Self { area, state }
    }

    pub fn set_callbacks<F, V>(&self, on_changed: F, on_view: V)
    where
        F: Fn() + 'static,
        V: Fn(ViewInfo) + 'static,
    {
        let mut s = self.state.borrow_mut();
        s.on_changed = Some(Rc::new(on_changed));
        s.on_view = Some(Rc::new(on_view));
    }

    pub fn set_note(&self, note: Option<Note>, pdf: Option<PdfDoc>) {
        {
            let mut s = self.state.borrow_mut();
            s.layout = note.as_ref().map(|n| n.layout()).unwrap_or_default();
            s.note = note;
            s.pdf = pdf;
            s.undo.clear();
            s.redo.clear();
            s.interaction = Interaction::None;
            s.cursor = None;
            s.pinching = false;
            s.touch_suppressed = false;
            s.path_cache.clear();
            s.pdf_pages.clear();
            s.pdf_scale = 0.0;
            s.cache_dirty = true;
            s.needs_fit = true;
            s.fit_camera();
            if s.width > 0 {
                s.needs_fit = false;
            }
        }
        self.area.queue_draw();
        emit_view(&self.state);
    }

    pub fn note(&self) -> Option<Note> {
        self.state.borrow().note.clone()
    }

    pub fn has_note(&self) -> bool {
        self.state.borrow().note.is_some()
    }

    pub fn set_title(&self, title: &str) {
        let mut s = self.state.borrow_mut();
        if let Some(note) = &mut s.note {
            note.title = title.to_string();
        }
    }

    pub fn add_page(&self) -> Option<usize> {
        let (index, changed) = {
            let mut s = self.state.borrow_mut();
            let Some(note) = &mut s.note else {
                return None;
            };
            if note.kind != NoteKind::Paged {
                return None;
            }
            note.add_page();
            note.touch();
            let index = note.pages.len() - 1;
            s.layout = note.layout();
            s.undo.push(Action::AddPage { index });
            s.redo.clear();
            s.cache_dirty = true;
            s.go_to_page(index);
            (index, true)
        };
        if changed {
            self.area.queue_draw();
            emit_view(&self.state);
            notify_changed(&self.state);
        }
        Some(index)
    }

    pub fn undo(&self) -> bool {
        let changed = {
            let mut s = self.state.borrow_mut();
            let action = match s.undo.pop() {
                Some(a) => a,
                None => return false,
            };
            let ok = s.apply_action(&action, false);
            if ok {
                s.redo.push(action);
                s.invalidate_ink();
                if let Some(note) = &mut s.note {
                    note.touch();
                }
                s.layout = s.note.as_ref().map(|n| n.layout()).unwrap_or_default();
            }
            ok
        };
        if changed {
            self.area.queue_draw();
            emit_view(&self.state);
            notify_changed(&self.state);
        }
        changed
    }

    pub fn redo(&self) -> bool {
        let changed = {
            let mut s = self.state.borrow_mut();
            let action = match s.redo.pop() {
                Some(a) => a,
                None => return false,
            };
            let ok = s.apply_action(&action, true);
            if ok {
                s.undo.push(action);
                s.invalidate_ink();
                if let Some(note) = &mut s.note {
                    note.touch();
                }
                s.layout = s.note.as_ref().map(|n| n.layout()).unwrap_or_default();
            }
            ok
        };
        if changed {
            self.area.queue_draw();
            emit_view(&self.state);
            notify_changed(&self.state);
        }
        changed
    }

    pub fn set_tool(&self, tool: Tool) {
        self.state.borrow_mut().tool = tool;
        self.area.queue_draw();
    }

    pub fn set_color(&self, color: &str) {
        self.state.borrow_mut().color = color.to_string();
    }

    pub fn set_pen_size(&self, size: f64) {
        self.state.borrow_mut().pen_size = size;
    }

    pub fn set_highlighter_size(&self, size: f64) {
        self.state.borrow_mut().highlighter_size = size;
    }

    pub fn set_eraser_size(&self, size: f64) {
        self.state.borrow_mut().eraser_size = size;
    }

    pub fn set_space_down(&self, down: bool) {
        self.state.borrow_mut().space_down = down;
    }

    pub fn zoom_in(&self) {
        let (sx, sy) = self.view_center();
        {
            let mut s = self.state.borrow_mut();
            let z = s.camera.zoom * 1.2;
            s.zoom_at(z, sx, sy);
        }
        self.area.queue_draw();
        emit_view(&self.state);
    }

    pub fn zoom_out(&self) {
        let (sx, sy) = self.view_center();
        {
            let mut s = self.state.borrow_mut();
            let z = s.camera.zoom / 1.2;
            s.zoom_at(z, sx, sy);
        }
        self.area.queue_draw();
        emit_view(&self.state);
    }

    pub fn zoom_reset(&self) {
        let (sx, sy) = self.view_center();
        {
            let mut s = self.state.borrow_mut();
            s.zoom_at(1.0, sx, sy);
        }
        self.area.queue_draw();
        emit_view(&self.state);
    }

    pub fn fit(&self) {
        {
            let mut s = self.state.borrow_mut();
            s.fit_camera();
            s.cache_dirty = true;
        }
        self.area.queue_draw();
        emit_view(&self.state);
    }

    pub fn step_page(&self, delta: isize) {
        {
            let mut s = self.state.borrow_mut();
            let current = s.current_page() as isize;
            let count = s.layout.len() as isize;
            if count == 0 {
                return;
            }
            let next = (current + delta).clamp(0, count - 1) as usize;
            s.go_to_page(next);
        }
        self.area.queue_draw();
        emit_view(&self.state);
    }

    pub fn view_info(&self) -> ViewInfo {
        self.state.borrow().view_info()
    }

    fn view_center(&self) -> (f64, f64) {
        let s = self.state.borrow();
        (s.width as f64 / 2.0, s.height as f64 / 2.0)
    }
}

impl State {
    fn apply_action(&mut self, action: &Action, forward: bool) -> bool {
        let Some(note) = &mut self.note else {
            return false;
        };
        match action {
            Action::AddStroke { page, index, stroke } => {
                let Some(p) = note.pages.get_mut(*page) else {
                    return false;
                };
                if forward {
                    let at = (*index).min(p.strokes.len());
                    p.strokes.insert(at, stroke.clone());
                } else {
                    if let Some(pos) = p.strokes.iter().position(|s| s.id == stroke.id) {
                        p.strokes.remove(pos);
                    } else {
                        return false;
                    }
                }
                true
            }
            Action::Erase { page, items } => {
                let Some(p) = note.pages.get_mut(*page) else {
                    return false;
                };
                if forward {
                    for (_, stroke) in items.iter().rev() {
                        if let Some(pos) = p.strokes.iter().position(|s| s.id == stroke.id) {
                            p.strokes.remove(pos);
                        }
                    }
                } else {
                    for (index, stroke) in items {
                        let at = (*index).min(p.strokes.len());
                        p.strokes.insert(at, stroke.clone());
                    }
                }
                true
            }
            Action::AddPage { index } => {
                if forward {
                    if note.kind != NoteKind::Paged {
                        return false;
                    }
                    while note.pages.len() <= *index {
                        note.pages.push(Page::default());
                        note.page_sizes.push([PAGE_WIDTH, PAGE_WIDTH * A4_RATIO]);
                    }
                } else {
                    if *index + 1 != note.pages.len() {
                        return false;
                    }
                    note.pages.pop();
                    note.page_sizes.pop();
                }
                true
            }
        }
    }
}

fn notify_changed(state: &Rc<RefCell<State>>) {
    let cb = state.borrow().on_changed.clone();
    if let Some(cb) = cb {
        cb();
    }
}

fn emit_view(state: &Rc<RefCell<State>>) {
    let (cb, info) = {
        let s = state.borrow();
        (s.on_view.clone(), s.view_info())
    };
    if let Some(cb) = cb {
        cb(info);
    }
}

fn pointer_down(s: &mut State, x: f64, y: f64, pressure: f64) {
    if s.note.is_none() || !matches!(&s.interaction, Interaction::None) {
        return;
    }
    if s.space_down || s.tool == Tool::Hand {
        s.interaction = Interaction::Pan {
            start: (x, y),
            camera: s.camera,
        };
        return;
    }
    let (wx, wy) = s.screen_to_world(x, y);
    let Some(note) = &s.note else {
        return;
    };
    let Some(page) = note.page_at(&s.layout, wx, wy) else {
        return;
    };
    let (cx, cy) = note.clamp_to_page(&s.layout, page, wx, wy);
    match s.tool {
        Tool::Pen | Tool::Highlighter => {
            let tool = if s.tool == Tool::Pen {
                StrokeTool::Pen
            } else {
                StrokeTool::Highlighter
            };
            let width = if tool == StrokeTool::Pen {
                s.pen_size
            } else {
                s.highlighter_size
            };
            let stroke = Stroke {
                id: next_stroke_id(),
                tool,
                color: s.color.clone(),
                width,
                points: vec![Point {
                    x: cx,
                    y: cy,
                    p: pressure,
                }],
            };
            s.interaction = Interaction::Stroke { page, stroke };
        }
        Tool::Eraser => {
            let hit = erase_hits(note, page, wx, wy, s.eraser_size / 2.0 / s.camera.zoom);
            s.interaction = Interaction::Erase { page, hit };
        }
        Tool::Hand => {}
    }
}

fn pointer_move(s: &mut State, x: f64, y: f64, pressure: f64) {
    let (wx, wy) = s.screen_to_world(x, y);
    match &mut s.interaction {
        Interaction::Stroke { page, stroke } => {
            let Some(note) = &s.note else {
                return;
            };
            let (cx, cy) = note.clamp_to_page(&s.layout, *page, wx, wy);
            let last = stroke.points.last().copied();
            let add = match last {
                None => true,
                Some(p) => {
                    let dx = cx - p.x;
                    let dy = cy - p.y;
                    dx * dx + dy * dy > 0.35 * 0.35 || (pressure - p.p).abs() > 0.03
                }
            };
            if add {
                stroke.points.push(Point {
                    x: cx,
                    y: cy,
                    p: pressure,
                });
            }
        }
        Interaction::Erase { page, hit } => {
            if let Some(note) = &s.note {
                let new_hits = erase_hits(
                    note,
                    *page,
                    wx,
                    wy,
                    s.eraser_size / 2.0 / s.camera.zoom,
                );
                for index in new_hits {
                    if !hit.contains(&index) {
                        hit.push(index);
                    }
                }
            }
        }
        Interaction::Pan { start, camera } => {
            s.camera.tx = camera.tx + (x - start.0);
            s.camera.ty = camera.ty + (y - start.1);
            s.cache_dirty = true;
        }
        Interaction::Pinch { .. } | Interaction::None => {}
    }
}

fn pointer_up(s: &mut State, x: f64, y: f64, _pressure: f64) -> bool {
    let interaction = std::mem::replace(&mut s.interaction, Interaction::None);
    match interaction {
        Interaction::Stroke { page, mut stroke } => {
            if stroke.points.len() == 1 {
                let p = stroke.points[0].clone();
                stroke.points.push(Point { x: p.x + 0.01, y: p.y + 0.01, p: p.p });
            }
            let Some(note) = &mut s.note else {
                return false;
            };
            let Some(page_data) = note.pages.get_mut(page) else {
                return false;
            };
            let index = page_data.strokes.len();
            page_data.strokes.push(stroke.clone());
            note.touch();
            s.undo.push(Action::AddStroke {
                page,
                index,
                stroke,
            });
            s.redo.clear();
            s.invalidate_ink();
            true
        }
        Interaction::Erase { page, mut hit } => {
            let (wx, wy) = s.screen_to_world(x, y);
            if let Some(note) = &s.note {
                let new_hits = erase_hits(
                    note,
                    page,
                    wx,
                    wy,
                    s.eraser_size / 2.0 / s.camera.zoom,
                );
                for index in new_hits {
                    if !hit.contains(&index) {
                        hit.push(index);
                    }
                }
            }
            if hit.is_empty() {
                s.cache_dirty = true;
                return false;
            }
            let Some(note) = &mut s.note else {
                return false;
            };
            let Some(page_data) = note.pages.get_mut(page) else {
                return false;
            };
            hit.sort_unstable();
            hit.dedup();
            let mut items = Vec::new();
            for index in hit.iter().rev() {
                if *index < page_data.strokes.len() {
                    items.push((*index, page_data.strokes.remove(*index)));
                }
            }
            items.sort_by_key(|(i, _)| *i);
            if items.is_empty() {
                return false;
            }
            note.touch();
            s.undo.push(Action::Erase { page, items });
            s.redo.clear();
            s.invalidate_ink();
            true
        }
        Interaction::Pan { .. } => {
            s.cache_dirty = true;
            false
        }
        Interaction::Pinch { .. } | Interaction::None => false,
    }
}

fn erase_hits(note: &Note, page: usize, x: f64, y: f64, radius: f64) -> Vec<usize> {
    let Some(p) = note.pages.get(page) else {
        return Vec::new();
    };
    p.strokes
        .iter()
        .enumerate()
        .filter(|(_, stroke)| stroke_hit(stroke, x, y, radius + stroke.width * 0.5))
        .map(|(i, _)| i)
        .collect()
}

fn stroke_hit(stroke: &Stroke, x: f64, y: f64, threshold: f64) -> bool {
    let t2 = threshold * threshold;
    if stroke.points.is_empty() {
        return false;
    }
    if stroke.points.len() == 1 {
        let p = &stroke.points[0];
        return (p.x - x).powi(2) + (p.y - y).powi(2) <= t2;
    }
    for pair in stroke.points.windows(2) {
        if segment_dist2(pair[0].x, pair[0].y, pair[1].x, pair[1].y, x, y) <= t2 {
            return true;
        }
    }
    false
}

fn segment_dist2(x1: f64, y1: f64, x2: f64, y2: f64, x: f64, y: f64) -> f64 {
    let dx = x2 - x1;
    let dy = y2 - y1;
    let len2 = dx * dx + dy * dy;
    if len2 < 1e-12 {
        return (x - x1).powi(2) + (y - y1).powi(2);
    }
    let t = (((x - x1) * dx + (y - y1) * dy) / len2).clamp(0.0, 1.0);
    let px = x1 + t * dx;
    let py = y1 + t * dy;
    (x - px).powi(2) + (y - py).powi(2)
}

#[allow(unused_must_use)]
fn draw(area: &gtk::DrawingArea, state: &Rc<RefCell<State>>, ctx: &Context, w: i32, h: i32) {
    let mut pending_view = false;
    {
        let mut s = state.borrow_mut();
        if s.width != w || s.height != h {
            s.width = w;
            s.height = h;
            s.cache = None;
            s.cache_dirty = true;
        }
        if s.needs_fit && w > 0 && h > 0 {
            s.fit_camera();
            s.needs_fit = false;
            pending_view = true;
        }
        let sf = area.scale_factor().max(1) as i32;
        let needs_cache = s
            .cache
            .as_ref()
            .map(|c| c.width() != w * sf || c.height() != h * sf)
            .unwrap_or(true);
        if needs_cache {
            s.cache = None;
            s.cache_dirty = true;
        }
        if s.cache_dirty || s.cache.is_none() {
            if let Ok(surface) =
                ImageSurface::create(Format::ARgb32, (w * sf).max(1), (h * sf).max(1))
            {
                if let Ok(cache_ctx) = Context::new(&surface) {
                    cache_ctx.set_antialias(cairo::Antialias::Best);
                    cache_ctx.scale(sf as f64, sf as f64);
                    s.draw_scene(&cache_ctx, w as f64, h as f64, sf as f64);
                    s.cache = Some(surface);
                    s.cache_dirty = false;
                }
            }
        }
        if let Some(cache) = &s.cache {
            ctx.save();
            ctx.scale(1.0 / sf as f64, 1.0 / sf as f64);
            let _ = ctx.set_source_surface(cache, 0.0, 0.0);
            let _ = ctx.paint();
            ctx.restore();
        } else {
            s.draw_scene(ctx, w as f64, h as f64, sf as f64);
        }
        s.draw_overlay(ctx);
    }
    if pending_view {
        emit_view(state);
    }
}

impl State {
    #[allow(unused_must_use)]
    fn draw_scene(&mut self, ctx: &Context, w: f64, h: f64, sf: f64) {
        let dark = libadwaita::StyleManager::default().is_dark();
        let Some(kind) = self.note.as_ref().map(|n| n.kind) else {
            ctx.set_source_rgb(0.92, 0.915, 0.895);
            let _ = ctx.paint();
            return;
        };

        if kind == NoteKind::Infinite {
            self.draw_infinite_background(ctx, w, h);
            ctx.save();
            ctx.translate(self.camera.tx, self.camera.ty);
            ctx.scale(self.camera.zoom, self.camera.zoom);
            self.draw_page_strokes(ctx, 0, None);
            ctx.restore();
        } else {
            if dark {
                ctx.set_source_rgb(0.075, 0.08, 0.09);
            } else {
                ctx.set_source_rgb(0.918, 0.914, 0.894);
            }
            let _ = ctx.paint();

            self.ensure_pdf(sf);
            ctx.save();
            ctx.translate(self.camera.tx, self.camera.ty);
            ctx.scale(self.camera.zoom, self.camera.zoom);

            let z = self.camera.zoom;
            let x0 = (0.0 - self.camera.tx) / z;
            let y0 = (0.0 - self.camera.ty) / z;
            let x1 = (w - self.camera.tx) / z;
            let y1 = (h - self.camera.ty) / z;
            let visible: Vec<usize> = self
                .layout
                .iter()
                .enumerate()
                .filter(|(_, r)| r.intersects(x0, y0, x1 - x0, y1 - y0))
                .map(|(i, _)| i)
                .collect();
            for &index in &visible {
                let rect = self.layout[index];
                self.draw_page_card(ctx, index, rect, dark);
            }
            ctx.restore();
            self.draw_page_labels(ctx, &visible, dark);
        }
    }

    fn draw_infinite_background(&self, ctx: &Context, w: f64, h: f64) {
        let gradient = cairo::RadialGradient::new(
            w * 0.5,
            h * 0.42,
            0.0,
            w * 0.5,
            h * 0.42,
            w.max(h) * 0.85,
        );
        gradient.add_color_stop_rgb(0.0, 0.988, 0.984, 0.969);
        gradient.add_color_stop_rgb(0.65, 0.973, 0.967, 0.949);
        gradient.add_color_stop_rgb(1.0, 0.949, 0.942, 0.922);
        let _ = ctx.set_source(&gradient);
        let _ = ctx.paint();

        let mut step = INFINITE_GRID * self.camera.zoom;
        while step < 13.0 {
            step *= 2.0;
        }
        let ox = self.camera.tx.rem_euclid(step);
        let oy = self.camera.ty.rem_euclid(step);
        ctx.set_source_rgba(0.10, 0.10, 0.11, 0.13);
        let mut x = ox;
        while x < w {
            let mut y = oy;
            while y < h {
                ctx.arc(x, y, 1.05, 0.0, TAU);
                let _ = ctx.fill();
                y += step;
            }
            x += step;
        }
    }

    #[allow(unused_must_use)]
    fn draw_page_card(&mut self, ctx: &Context, index: usize, rect: Rect, dark: bool) {
        let radius = 7.0;
        // Soft shadow.
        ctx.set_source_rgba(
            0.0,
            0.0,
            0.0,
            if dark { 0.42 } else { 0.075 },
        );
        rounded_rect(ctx, rect.x + 1.0, rect.y + 6.0, rect.w, rect.h, radius);
        let _ = ctx.fill();
        ctx.set_source_rgba(0.0, 0.0, 0.0, if dark { 0.18 } else { 0.035 });
        rounded_rect(ctx, rect.x - 4.0, rect.y + 10.0, rect.w + 8.0, rect.h, radius + 6.0);
        let _ = ctx.fill();

        // Paper.
        ctx.set_source_rgb(0.998, 0.997, 0.991);
        rounded_rect(ctx, rect.x, rect.y, rect.w, rect.h, radius);
        let _ = ctx.fill();

        if self.note.as_ref().map(|n| n.kind) == Some(NoteKind::Pdf) {
            if let Some(page) = self.pdf_pages.get(&index) {
                let sw = page.surface.width() as f64;
                let sh = page.surface.height() as f64;
                if sw > 0.0 && sh > 0.0 {
                    ctx.save();
                    rounded_rect(ctx, rect.x, rect.y, rect.w, rect.h, radius);
                    ctx.clip();
                    ctx.new_path();
                    ctx.translate(rect.x, rect.y);
                    ctx.scale(rect.w / sw, rect.h / sh);
                    let _ = ctx.set_source_surface(&page.surface, 0.0, 0.0);
                    let _ = ctx.paint();
                    ctx.restore();
                }
            } else {
                // Keep the page blank while the PDF surface is prepared.
                let _ = index;
            }
        }

        // Ink, clipped to the sheet.
        ctx.save();
        rounded_rect(ctx, rect.x, rect.y, rect.w, rect.h, radius);
        ctx.clip();
        ctx.new_path();
        self.draw_page_strokes(ctx, index, Some(rect));
        ctx.restore();

        // Hairline border.
        ctx.set_source_rgba(0.0, 0.0, 0.0, if dark { 0.25 } else { 0.05 });
        ctx.set_line_width(1.0 / self.camera.zoom);
        rounded_rect(ctx, rect.x + 0.5, rect.y + 0.5, rect.w - 1.0, rect.h - 1.0, radius);
        let _ = ctx.stroke();
    }

    fn draw_page_labels(&self, ctx: &Context, visible: &[usize], dark: bool) {
        let z = self.camera.zoom;
        for &index in visible {
            let rect = self.layout[index];
            let sx = rect.x * z + self.camera.tx;
            let sy = rect.y * z + self.camera.ty - 9.0;
            if sy < 8.0 {
                continue;
            }
            ctx.select_font_face("Adwaita Sans", FontSlant::Normal, FontWeight::Normal);
            ctx.set_font_size(11.0);
            if dark {
                ctx.set_source_rgba(0.8, 0.8, 0.82, 0.55);
            } else {
                ctx.set_source_rgba(0.25, 0.25, 0.26, 0.45);
            }
            ctx.move_to(sx + 2.0, sy);
            let _ = ctx.show_text(&format!("{}", index + 1));
        }
    }

    fn ensure_pdf(&mut self, sf: f64) {
        if self.note.as_ref().map(|n| n.kind) != Some(NoteKind::Pdf) {
            return;
        }
        let Some(pdf) = &self.pdf else {
            return;
        };
        let target = (self.camera.zoom * sf).clamp(0.45, 2.5);
        if self.pdf_scale <= 0.0
            || target > self.pdf_scale * 1.25
            || target < self.pdf_scale * 0.42
        {
            self.pdf_pages.clear();
            self.pdf_scale = target;
        }
        let z = self.camera.zoom;
        let x0 = (0.0 - self.camera.tx) / z;
        let y0 = (0.0 - self.camera.ty) / z;
        let w = self.width as f64;
        let h = self.height as f64;
        let x1 = (w - self.camera.tx) / z;
        let y1 = (h - self.camera.ty) / z;
        let visible: Vec<usize> = self
            .layout
            .iter()
            .enumerate()
            .filter(|(_, r)| r.intersects(x0, y0, x1 - x0, y1 - y0))
            .map(|(i, _)| i)
            .collect();
        for &index in &visible {
            if !self.pdf_pages.contains_key(&index) {
                if let Some(surface) = pdf.render(index, PAGE_WIDTH * target) {
                    self.pdf_pages.insert(index, CachedPdf { surface });
                }
            }
        }
        self.pdf_pages.retain(|i, _| visible.contains(i));
    }

    fn ensure_path(&mut self, page: usize, index: usize) {
        if self.path_cache.contains_key(&(page, index)) {
            return;
        }
        let stroke = self
            .note
            .as_ref()
            .and_then(|n| n.pages.get(page))
            .and_then(|p| p.strokes.get(index))
            .cloned();
        let Some(stroke) = stroke else {
            return;
        };
        self.scratch_ctx.new_path();
        build_stroke_path(&self.scratch_ctx, &stroke);
        if let Ok(path) = self.scratch_ctx.copy_path() {
            self.path_cache.insert((page, index), path);
        }
    }

    #[allow(unused_must_use)]
    fn draw_page_strokes(&mut self, ctx: &Context, page: usize, _clip: Option<Rect>) {
        let count = self
            .note
            .as_ref()
            .and_then(|n| n.pages.get(page))
            .map(|p| p.strokes.len())
            .unwrap_or(0);
        for index in 0..count {
            self.ensure_path(page, index);
        }
        let Some(note) = &self.note else {
            return;
        };
        let Some(page_data) = note.pages.get(page) else {
            return;
        };
        for (index, stroke) in page_data.strokes.iter().enumerate() {
            if let Some(path) = self.path_cache.get(&(page, index)) {
                paint_stroke(ctx, stroke, path, 1.0);
            }
        }
    }

    #[allow(unused_must_use)]
    fn draw_overlay(&mut self, ctx: &Context) {
        if self.note.is_none() {
            return;
        }
        let live = match &self.interaction {
            Interaction::Stroke { page, stroke } => {
                Some((*page, LiveItem::Stroke(stroke.clone())))
            }
            Interaction::Erase { page, hit } => {
                Some((*page, LiveItem::Erase(hit.clone())))
            }
            _ => None,
        };
        // Live stroke / eraser feedback in world space.
        let (tx, ty, zoom) = (self.camera.tx, self.camera.ty, self.camera.zoom);
        ctx.save();
        ctx.translate(tx, ty);
        ctx.scale(zoom, zoom);
        if let Some((page, item)) = live {
            let infinite = self
                .note
                .as_ref()
                .map(|n| n.kind == NoteKind::Infinite)
                .unwrap_or(false);
            let clip = if infinite { None } else { self.layout.get(page).copied() };
            if let Some(rect) = clip {
                ctx.save();
                rounded_rect(ctx, rect.x, rect.y, rect.w, rect.h, 7.0);
                ctx.clip();
                ctx.new_path();
            }
            match item {
                LiveItem::Stroke(stroke) => {
                    self.scratch_ctx.new_path();
                    build_stroke_path(&self.scratch_ctx, &stroke);
                    if let Ok(path) = self.scratch_ctx.copy_path() {
                        paint_stroke(ctx, &stroke, &path, 1.0);
                    }
                }
                LiveItem::Erase(hit) => {
                    for index in &hit {
                        self.ensure_path(page, *index);
                    }
                    if let Some(note) = &self.note {
                        if let Some(page_data) = note.pages.get(page) {
                            for index in hit {
                                if let Some(stroke) = page_data.strokes.get(index) {
                                    if let Some(path) = self.path_cache.get(&(page, index)) {
                                        ctx.new_path();
                                        ctx.append_path(path);
                                        let (r, g, b) = hex_to_cairo(&stroke.color);
                                        ctx.set_source_rgba(r, g, b, 0.18);
                                        let _ = ctx.fill();
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if clip.is_some() {
                ctx.restore();
            }
        }
        ctx.restore();

        // Cursor.
        if let Some((x, y)) = self.cursor {
            let hand = self.space_down || self.tool == Tool::Hand;
            let radius = if hand {
                7.0
            } else {
                match self.tool {
                    Tool::Pen => (self.pen_size * zoom * 0.5).max(2.0),
                    Tool::Highlighter => (self.highlighter_size * zoom * 0.5).max(4.0),
                    Tool::Eraser => self.eraser_size * 0.5,
                    Tool::Hand => 7.0,
                }
            };
            ctx.set_line_width(1.4);
            ctx.arc(x, y, radius, 0.0, TAU);
            ctx.set_source_rgba(1.0, 1.0, 1.0, 0.85);
            let _ = ctx.stroke_preserve();
            ctx.set_source_rgba(0.0, 0.0, 0.0, 0.32);
            let _ = ctx.stroke();
            if !hand && radius > 6.0 {
                ctx.arc(x, y, 1.1, 0.0, TAU);
                ctx.set_source_rgba(0.0, 0.0, 0.0, 0.4);
                let _ = ctx.fill();
            }
        }
    }
}

fn paint_stroke(ctx: &Context, stroke: &Stroke, path: &Path, alpha: f64) {
    let (r, g, b) = hex_to_cairo(&stroke.color);
    if stroke.tool == StrokeTool::Highlighter {
        ctx.set_source_rgba(r, g, b, 0.30 * alpha);
    } else {
        ctx.set_source_rgba(r, g, b, alpha);
    }
    ctx.new_path();
    ctx.append_path(path);
    ctx.set_line_width(stroke.width.max(0.4));
    ctx.set_line_cap(cairo::LineCap::Round);
    ctx.set_line_join(cairo::LineJoin::Round);
    let _ = ctx.stroke();
}

fn rounded_rect(ctx: &Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(w * 0.5).min(h * 0.5).max(0.0);
    ctx.new_sub_path();
    ctx.arc(x + w - r, y + r, r, -PI * 0.5, 0.0);
    ctx.arc(x + w - r, y + h - r, r, 0.0, PI * 0.5);
    ctx.arc(x + r, y + h - r, r, PI * 0.5, PI);
    ctx.arc(x + r, y + r, r, PI, PI * 1.5);
    ctx.close_path();
}

fn build_stroke_path(ctx: &Context, stroke: &Stroke) {
    let samples = stroke_samples(stroke);
    if samples.is_empty() {
        return;
    }
    if samples.len() == 1 {
        let (x, y) = samples[0];
        // A tiny segment plus round caps renders as a dot.
        ctx.move_to(x, y);
        ctx.line_to(x + 0.01, y);
        return;
    }
    append_smooth_polyline(ctx, &samples);
}

/// Append a Catmull-Rom spline through the points as cubic Beziers.
fn append_smooth_polyline(ctx: &Context, points: &[(f64, f64)]) {
    if points.is_empty() {
        return;
    }
    ctx.move_to(points[0].0, points[0].1);
    if points.len() == 1 {
        return;
    }
    for i in 1..points.len() {
        let p0 = points[i.saturating_sub(2)];
        let p1 = points[i - 1];
        let p2 = points[i];
        let p3 = points[(i + 1).min(points.len() - 1)];
        let c1x = p1.0 + (p2.0 - p0.0) / 6.0;
        let c1y = p1.1 + (p2.1 - p0.1) / 6.0;
        let c2x = p2.0 - (p3.0 - p1.0) / 6.0;
        let c2y = p2.1 - (p3.1 - p1.1) / 6.0;
        ctx.curve_to(c1x, c1y, c2x, c2y, p2.0, p2.1);
    }
}

fn stroke_samples(stroke: &Stroke) -> Vec<(f64, f64)> {
    let points = smooth_points(&stroke.points);
    if points.is_empty() {
        return Vec::new();
    }
    if points.len() == 1 {
        return vec![(points[0].x, points[0].y)];
    }

    let mut out = Vec::new();
    let n = points.len();
    for i in 0..n - 1 {
        let p0 = points[i.saturating_sub(1)];
        let p1 = points[i];
        let p2 = points[i + 1];
        let p3 = points[(i + 2).min(n - 1)];
        let dx = p2.x - p1.x;
        let dy = p2.y - p1.y;
        let dist = (dx * dx + dy * dy).sqrt();
        // Dense sampling plus a centripetal spline keeps fast strokes smooth.
        let steps = ((dist / 0.8).ceil() as usize).clamp(1, 240);
        for step in 0..steps {
            let t = step as f64 / steps as f64;
            out.push(catmull_centripetal(p0, p1, p2, p3, t));
        }
    }
    let last = points[n - 1];
    out.push((last.x, last.y));
    out
}

/// Smooth sparse, uneven pen samples before interpolation. Fast strokes
/// produce fewer points per distance, so a light moving average removes the
/// angular kinks that would otherwise survive the spline.
fn smooth_points(points: &[Point]) -> Vec<Point> {
    if points.len() <= 2 {
        return points.to_vec();
    }

    let mut spacing = 0.0;
    for pair in points.windows(2) {
        let dx = pair[1].x - pair[0].x;
        let dy = pair[1].y - pair[0].y;
        spacing += (dx * dx + dy * dy).sqrt();
    }
    spacing /= (points.len() - 1) as f64;
    let passes = if spacing > 6.0 { 2 } else { 1 };

    let mut out = points.to_vec();
    for _ in 0..passes {
        let src = out.clone();
        for i in 1..src.len() - 1 {
            out[i].x = src[i - 1].x * 0.25 + src[i].x * 0.5 + src[i + 1].x * 0.25;
            out[i].y = src[i - 1].y * 0.25 + src[i].y * 0.5 + src[i + 1].y * 0.25;
        }
    }
    out
}

/// Centripetal Catmull-Rom interpolation. Unlike the uniform variant, this
/// avoids cusps and overshoot when pen samples are unevenly spaced, which is
/// exactly what happens when writing quickly.
fn catmull_centripetal(p0: Point, p1: Point, p2: Point, p3: Point, t: f64) -> (f64, f64) {
    fn knot(a: Point, b: Point) -> f64 {
        let dx = b.x - a.x;
        let dy = b.y - a.y;
        (dx * dx + dy * dy).sqrt().sqrt().max(1e-4)
    }
    fn lerp(a: Point, b: Point, t: f64) -> Point {
        Point {
            x: a.x + (b.x - a.x) * t,
            y: a.y + (b.y - a.y) * t,
            p: 1.0,
        }
    }

    let t0 = 0.0;
    let t1 = t0 + knot(p0, p1);
    let t2 = t1 + knot(p1, p2);
    let t3 = t2 + knot(p2, p3);
    let tt = t1 + (t2 - t1) * t;

    let a1 = lerp(p0, p1, (tt - t0) / (t1 - t0));
    let a2 = lerp(p1, p2, (tt - t1) / (t2 - t1));
    let a3 = lerp(p2, p3, (tt - t2) / (t3 - t2));
    let b1 = lerp(a1, a2, (tt - t0) / (t2 - t0));
    let b2 = lerp(a2, a3, (tt - t1) / (t3 - t1));
    let c = lerp(b1, b2, (tt - t1) / (t2 - t1));
    (c.x, c.y)
}

