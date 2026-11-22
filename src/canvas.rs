use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::f64::consts::{PI, TAU};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use cairo::{Context, FontSlant, FontWeight, Format, ImageSurface, Path};
use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;

use crate::model::{
    hex_to_cairo, Note, NoteKind, Page, Point, Rect, ShapeKind, Stroke, StrokeTool, A4_RATIO,
    INFINITE_GRID, PAGE_WIDTH,
};
use crate::pdf::PdfDoc;

pub(crate) fn input_debug_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("SUMI_DEBUG_INPUT").is_some())
}

pub(crate) fn device_looks_like_stylus(device: &gdk::Device) -> bool {
    if device.source() == gdk::InputSource::Pen {
        return true;
    }
    // Some AES / Bluetooth pens are exposed as a generic mouse or keyboard.
    let name = device.name().to_lowercase();
    name.contains("pen") || name.contains("stylus") || name.contains("eraser")
}

pub(crate) fn input_debug_log(message: &str) {
    eprintln!("{message}");
    use std::io::Write;
    let Some(dir) = dirs::data_dir().map(|dir| dir.join("sumi")) else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("input-debug.log"))
    {
        let _ = writeln!(file, "{message}");
    }
}

pub(crate) fn log_input_event(event: &gdk::Event) {
    if !input_debug_enabled() {
        return;
    }
    let device = event.device();
    let source = device.as_ref().map(|d| d.source());
    let name = device.as_ref().map(|d| d.name().to_string());
    let tool = event.device_tool();
    let button = event.downcast_ref::<gdk::ButtonEvent>().map(|e| e.button());
    input_debug_log(&format!(
        "[sumi:input] type={:?} source={:?} device={:?} tool={:?} button={:?} mods={:?} pos={:?} pressure={:?} tilt={:?}",
        event.event_type(),
        source,
        name,
        tool.as_ref()
            .map(|t| (t.tool_type(), t.axes(), t.serial(), t.hardware_id())),
        button,
        event.modifier_state(),
        event.position(),
        event.axis(gdk::AxisUse::Pressure),
        (
            event.axis(gdk::AxisUse::Xtilt),
            event.axis(gdk::AxisUse::Ytilt),
        ),
    ));
}

static NEXT_STROKE_ID: AtomicU64 = AtomicU64::new(1);

fn next_stroke_id() -> u64 {
    NEXT_STROKE_ID.fetch_add(1, Ordering::Relaxed)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Pen,
    Highlighter,
    Eraser,
    Shape,
    Select,
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
    Select {
        page: usize,
        start: (f64, f64),
        current: (f64, f64),
        additive: bool,
    },
    Shape {
        page: usize,
        start: (f64, f64),
        current: (f64, f64),
    },
    MoveSelection {
        page: usize,
        start: (f64, f64),
        delta: (f64, f64),
        moved: bool,
        ids: Vec<u64>,
        originals: Vec<(usize, Stroke)>,
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
    AddStrokes {
        page: usize,
        items: Vec<(usize, Stroke)>,
    },
    MoveStrokes {
        page: usize,
        ids: Vec<u64>,
        dx: f64,
        dy: f64,
    },
    RecolorStrokes {
        page: usize,
        color: String,
        previous: Vec<(u64, String)>,
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
    shape_kind: ShapeKind,

    interaction: Interaction,
    cursor: Option<(f64, f64)>,
    space_down: bool,
    pinching: bool,
    touch_suppressed: bool,

    /// Temporary tool selected by a stylus barrel button or the eraser tip.
    stylus_tool: Option<Tool>,
    /// Barrel buttons currently held, in press order (GDK button numbers).
    stylus_buttons: Vec<u32>,

    undo: Vec<Action>,
    redo: Vec<Action>,

    selection: Vec<u64>,
    selection_page: Option<usize>,
    clipboard: Vec<Stroke>,

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
        let scratch_surface = ImageSurface::create(Format::ARgb32, 4, 4).expect("scratch surface");
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
            shape_kind: ShapeKind::Line,
            interaction: Interaction::None,
            cursor: None,
            space_down: false,
            pinching: false,
            touch_suppressed: false,
            stylus_tool: None,
            stylus_buttons: Vec::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            selection: Vec::new(),
            selection_page: None,
            clipboard: Vec::new(),
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

    fn active_tool(&self) -> Tool {
        self.stylus_tool.unwrap_or(self.tool)
    }

    #[allow(dead_code)]
    fn active_size(&self) -> f64 {
        match self.active_tool() {
            Tool::Pen => self.pen_size,
            Tool::Highlighter => self.highlighter_size,
            Tool::Eraser => self.eraser_size,
            Tool::Shape => self.pen_size,
            Tool::Select | Tool::Hand => self.pen_size,
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

    fn clear_selection(&mut self) {
        self.selection.clear();
        self.selection_page = None;
    }

    /// Recolor the selected strokes of one tool group.
    /// `highlighter` picks the marker strokes, otherwise the pen strokes.
    fn recolor_selection(&mut self, color: &str, highlighter: bool) -> bool {
        let Some(page) = self.selection_page else {
            return false;
        };
        let ids = self.selection.clone();
        let Some(note) = &mut self.note else {
            return false;
        };
        let Some(page_data) = note.pages.get_mut(page) else {
            return false;
        };
        let mut previous = Vec::new();
        for stroke in &mut page_data.strokes {
            if !ids.contains(&stroke.id) {
                continue;
            }
            if (stroke.tool == StrokeTool::Highlighter) != highlighter {
                continue;
            }
            if stroke.color == color {
                continue;
            }
            previous.push((stroke.id, stroke.color.clone()));
            stroke.color = color.to_string();
        }
        if previous.is_empty() {
            return false;
        }
        note.touch();
        self.undo.push(Action::RecolorStrokes {
            page,
            color: color.to_string(),
            previous,
        });
        self.redo.clear();
        self.invalidate_ink();
        true
    }

    /// Whether a stroke of this colour and tool group is selected, so the
    /// palette can mark the colours the selection already uses.
    fn selection_has_color(&self, color: &str, highlighter: bool) -> bool {
        let Some(page) = self.selection_page else {
            return false;
        };
        let Some(note) = &self.note else {
            return false;
        };
        let Some(page_data) = note.pages.get(page) else {
            return false;
        };
        page_data.strokes.iter().any(|stroke| {
            self.selection.contains(&stroke.id)
                && stroke.color == color
                && (stroke.tool == StrokeTool::Highlighter) == highlighter
        })
    }

    fn prune_selection(&mut self) {
        let Some(page) = self.selection_page else {
            self.selection.clear();
            return;
        };
        let Some(note) = &self.note else {
            self.clear_selection();
            return;
        };
        let Some(page_data) = note.pages.get(page) else {
            self.clear_selection();
            return;
        };
        self.selection
            .retain(|id| page_data.strokes.iter().any(|stroke| stroke.id == *id));
        if self.selection.is_empty() {
            self.selection_page = None;
        }
    }

    fn selection_indices(&self, page: usize) -> Vec<usize> {
        let Some(note) = &self.note else {
            return Vec::new();
        };
        let Some(page_data) = note.pages.get(page) else {
            return Vec::new();
        };
        page_data
            .strokes
            .iter()
            .enumerate()
            .filter(|(_, stroke)| self.selection.contains(&stroke.id))
            .map(|(index, _)| index)
            .collect()
    }

    fn selection_origin(&self, page: usize) -> (f64, f64) {
        self.layout
            .get(page)
            .map(|rect| (rect.x, rect.y))
            .unwrap_or((0.0, 0.0))
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
        let texture =
            gdk::MemoryTexture::new(1, 1, gdk::MemoryFormat::R8g8b8a8Premultiplied, &blank, 4);
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
                let eraser_tip = g
                    .device_tool()
                    .map(|tool| tool.tool_type() == gdk::DeviceToolType::Eraser)
                    .unwrap_or(false);
                if let Some(area) = area_w.upgrade() {
                    area.grab_focus();
                    let mut s = state.borrow_mut();
                    // A barrel button held before the tip touches down takes
                    // precedence, then the eraser end of the pen.
                    if eraser_tip {
                        s.stylus_tool = Some(Tool::Eraser);
                    } else if s.stylus_tool.is_none() {
                        s.stylus_tool = stylus_tool_for_event_state(g.current_event_state());
                    }
                    s.cursor = Some((x, y));
                    let additive = g
                        .current_event_state()
                        .contains(gdk::ModifierType::SHIFT_MASK);
                    pointer_down(&mut s, x, y, pressure, additive);
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
                        let changed = pointer_up(&mut s, x, y, pressure);
                        // The eraser tip override ends with the contact. If a
                        // barrel button is still held, keep its temporary tool.
                        s.stylus_tool = stylus_tool_for_buttons(&s.stylus_buttons);
                        changed
                    };
                    area.queue_draw();
                    if changed {
                        notify_changed(&state);
                    }
                    // Selection changes are not content edits, but the palette
                    // marks the colours the selection uses.
                    emit_view(&state);
                }
            }
        });
        area.add_controller(stylus);

        // Barrel buttons on the stylus. GTK only routes the tip through
        // GestureStylus, so track the extra buttons ourselves.
        let barrel = gtk::EventControllerLegacy::new();
        barrel.set_propagation_phase(gtk::PropagationPhase::Capture);
        barrel.connect_event({
            let state = state.clone();
            let area_w = area.downgrade();
            move |_, event| {
                log_input_event(event);
                // Some pens (including AES/Bluetooth pens) are exposed as a
                // generic mouse rather than GDK_SOURCE_PEN. Any event that
                // carries a device tool, or a device that looks like a pen,
                // is still treated as a stylus.
                let is_stylus = event.device_tool().is_some()
                    || event
                        .device()
                        .map(|device| device_looks_like_stylus(&device))
                        .unwrap_or(false);
                if !is_stylus {
                    return glib::Propagation::Proceed;
                }

                match event.event_type() {
                    gdk::EventType::ButtonPress => {
                        let button = event
                            .downcast_ref::<gdk::ButtonEvent>()
                            .map(|button_event| button_event.button())
                            .unwrap_or(0);
                        // Button 1 is the tip (handled by GestureStylus).
                        // Stylus barrel buttons use backend-dependent numbers,
                        // so treat any other pen button as a barrel button.
                        if button <= 1 {
                            return glib::Propagation::Proceed;
                        }
                        let Some((x, y)) = event.position() else {
                            return glib::Propagation::Stop;
                        };
                        let pressure = event
                            .axis(gdk::AxisUse::Pressure)
                            .unwrap_or(0.5)
                            .clamp(0.05, 1.0);
                        let mut committed = false;
                        if let Some(area) = area_w.upgrade() {
                            {
                                let mut s = state.borrow_mut();
                                if !s.stylus_buttons.contains(&button) {
                                    s.stylus_buttons.push(button);
                                }
                                let was_eraser = s.stylus_tool == Some(Tool::Eraser);
                                s.stylus_tool = stylus_tool_for_buttons(&s.stylus_buttons);
                                s.cursor = Some((x, y));
                                // Pressing the eraser button in the middle of a
                                // stroke switches straight to erasing.
                                if !was_eraser
                                    && s.stylus_tool == Some(Tool::Eraser)
                                    && matches!(&s.interaction, Interaction::Stroke { .. })
                                {
                                    committed = pointer_up(&mut s, x, y, pressure);
                                    let additive = event
                                        .modifier_state()
                                        .contains(gdk::ModifierType::SHIFT_MASK);
                                    pointer_down(&mut s, x, y, pressure, additive);
                                }
                            }
                            area.queue_draw();
                            if committed {
                                notify_changed(&state);
                                emit_view(&state);
                            }
                        }
                        glib::Propagation::Stop
                    }
                    gdk::EventType::ButtonRelease => {
                        let button = event
                            .downcast_ref::<gdk::ButtonEvent>()
                            .map(|button_event| button_event.button())
                            .unwrap_or(0);
                        if button <= 1 {
                            return glib::Propagation::Proceed;
                        }
                        if let Some(area) = area_w.upgrade() {
                            let mut s = state.borrow_mut();
                            s.stylus_buttons.retain(|held| *held != button);
                            // Keep the current tool while a stroke is still in
                            // progress; the stylus-up handler clears it.
                            if matches!(&s.interaction, Interaction::None) {
                                s.stylus_tool = stylus_tool_for_buttons(&s.stylus_buttons);
                            }
                            drop(s);
                            area.queue_draw();
                        }
                        glib::Propagation::Stop
                    }
                    gdk::EventType::ProximityOut => {
                        let mut s = state.borrow_mut();
                        s.stylus_buttons.clear();
                        s.stylus_tool = None;
                        glib::Propagation::Proceed
                    }
                    _ => glib::Propagation::Proceed,
                }
            }
        });
        area.add_controller(barrel);

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
                    if source == Some(gdk::InputSource::Touchscreen) {
                        // Let GestureZoom own touch sequences. Denying the drag
                        // here may synchronously emit drag-end, so the canvas
                        // state must not be borrowed yet.
                        g.set_state(gtk::EventSequenceState::Denied);
                        return;
                    }
                    let mut s = state.borrow_mut();
                    match source {
                        Some(gdk::InputSource::Pen) | Some(gdk::InputSource::TabletPad) => {
                            // The stylus gesture handles these when available.
                        }
                        _ => {
                            s.cursor = Some((x, y));
                            let additive = g
                                .current_event_state()
                                .contains(gdk::ModifierType::SHIFT_MASK);
                            pointer_down(&mut s, x, y, 0.5, additive);
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
                    }
                    emit_view(&state);
                }
            }
        });
        area.add_controller(drag);

        // Mouse clicks select without starting a drag. Pen and touch are
        // handled by the stylus/zoom gestures above.
        let click = gtk::GestureClick::new();
        click.set_button(1);
        click.connect_pressed({
            let state = state.clone();
            let area_w = area.downgrade();
            move |g, _n, x, y| {
                let source = g.current_event_device().map(|d| d.source());
                if source != Some(gdk::InputSource::Mouse) {
                    return;
                }
                let additive = g
                    .current_event_state()
                    .contains(gdk::ModifierType::SHIFT_MASK);
                {
                    let mut s = state.borrow_mut();
                    if s.active_tool() != Tool::Select
                        || !matches!(s.interaction, Interaction::None)
                    {
                        return;
                    }
                    select_click(&mut s, x, y, additive);
                }
                if let Some(area) = area_w.upgrade() {
                    area.queue_draw();
                }
                emit_view(&state);
            }
        });
        area.add_controller(click);

        // Middle-button drag pans.
        let middle = gtk::GestureDrag::new();
        middle.set_button(2);
        middle.connect_drag_begin({
            let state = state.clone();
            let area_w = area.downgrade();
            move |g, x, y| {
                // A stylus barrel button also reports as a middle click.
                let is_pen = g
                    .current_event_device()
                    .map(|device| device_looks_like_stylus(&device))
                    .unwrap_or(false);
                if is_pen {
                    // Denying may synchronously emit drag-end, so do not
                    // borrow the canvas state yet.
                    g.set_state(gtk::EventSequenceState::Denied);
                    return;
                }
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
                if matches!(
                    &s.interaction,
                    Interaction::Stroke { .. } | Interaction::Erase { .. }
                ) {
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
                let center = g.bounding_box_center().unwrap_or((0.0, 0.0));
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
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
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
                    let zooming = mods
                        .intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::META_MASK);
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
            let mut note = note;
            if let Some(note) = &mut note {
                // Keep new ids above anything already persisted in the note.
                // The process-local counter restarts at 1 on every launch.
                let max_id = note
                    .pages
                    .iter()
                    .flat_map(|page| page.strokes.iter())
                    .map(|stroke| stroke.id)
                    .max()
                    .unwrap_or(0);
                NEXT_STROKE_ID.fetch_max(max_id.saturating_add(1), Ordering::Relaxed);

                // Older notes may contain duplicate or missing stroke ids.
                // Assign stable ids so selection and undo can address strokes.
                let mut seen = HashSet::new();
                for page in &mut note.pages {
                    for stroke in &mut page.strokes {
                        if stroke.id == 0 || !seen.insert(stroke.id) {
                            stroke.id = next_stroke_id();
                            seen.insert(stroke.id);
                        }
                    }
                }
            }
            s.layout = note.as_ref().map(|n| n.layout()).unwrap_or_default();
            s.note = note;
            s.pdf = pdf;
            s.undo.clear();
            s.redo.clear();
            s.clear_selection();
            s.interaction = Interaction::None;
            s.cursor = None;
            s.pinching = false;
            s.touch_suppressed = false;
            s.stylus_tool = None;
            s.stylus_buttons.clear();
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
                s.prune_selection();
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
                s.prune_selection();
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
        {
            let mut s = self.state.borrow_mut();
            s.tool = tool;
            if tool != Tool::Select {
                s.clear_selection();
            }
        }
        self.area.queue_draw();
    }

    pub fn clear_selection(&self) {
        self.state.borrow_mut().clear_selection();
        self.area.queue_draw();
        emit_view(&self.state);
    }

    pub fn select_all(&self) {
        {
            let mut s = self.state.borrow_mut();
            let page = s.current_page();
            let Some(note) = &s.note else { return };
            let Some(page_data) = note.pages.get(page) else {
                return;
            };
            s.selection = page_data.strokes.iter().map(|stroke| stroke.id).collect();
            s.selection_page = if s.selection.is_empty() {
                None
            } else {
                Some(page)
            };
        }
        self.area.queue_draw();
    }

    pub fn copy_selection(&self) -> bool {
        let mut s = self.state.borrow_mut();
        let Some(page) = s.selection_page else {
            return false;
        };
        let Some(note) = &s.note else {
            return false;
        };
        let Some(page_data) = note.pages.get(page) else {
            return false;
        };
        let (ox, oy) = s.selection_origin(page);
        let strokes: Vec<Stroke> = page_data
            .strokes
            .iter()
            .filter(|stroke| s.selection.contains(&stroke.id))
            .map(|stroke| {
                let mut stroke = stroke.clone();
                for point in &mut stroke.points {
                    point.x -= ox;
                    point.y -= oy;
                }
                stroke
            })
            .collect();
        if strokes.is_empty() {
            return false;
        }
        s.clipboard = strokes;
        true
    }

    pub fn paste_clipboard(&self) -> bool {
        let changed = {
            let mut s = self.state.borrow_mut();
            if s.clipboard.is_empty() {
                return false;
            }
            let Some(note_ref) = &s.note else {
                return false;
            };
            let (sx, sy) = s
                .cursor
                .unwrap_or((s.width as f64 * 0.5, s.height as f64 * 0.5));
            let (wx, wy) = s.screen_to_world(sx, sy);
            let pointer_page = note_ref.page_at(&s.layout, wx, wy);
            let page = pointer_page.unwrap_or_else(|| s.current_page());
            let (ox, oy) = s.selection_origin(page);
            let Some(bounds) = clipboard_bounds(&s.clipboard) else {
                return false;
            };
            let target = s.layout.get(page).copied();
            let mut dx = if pointer_page == Some(page) {
                wx - (ox + bounds.x + bounds.w * 0.5)
            } else {
                24.0
            };
            let mut dy = if pointer_page == Some(page) {
                wy - (oy + bounds.y + bounds.h * 0.5)
            } else {
                24.0
            };
            if let Some(rect) = target {
                let world_bounds = Rect {
                    x: ox + bounds.x,
                    y: oy + bounds.y,
                    ..bounds
                };
                (dx, dy) = clamp_translation(world_bounds, rect, dx, dy);
            }
            let clipboard = s.clipboard.clone();
            let Some(note) = &mut s.note else {
                return false;
            };
            let Some(page_data) = note.pages.get_mut(page) else {
                return false;
            };
            let start = page_data.strokes.len();
            let mut items = Vec::with_capacity(clipboard.len());
            let mut ids = Vec::with_capacity(clipboard.len());
            for (offset, original) in clipboard.iter().enumerate() {
                let mut stroke = original.clone();
                stroke.id = next_stroke_id();
                for point in &mut stroke.points {
                    point.x += ox + dx;
                    point.y += oy + dy;
                }
                ids.push(stroke.id);
                items.push((start + offset, stroke.clone()));
                page_data.strokes.push(stroke);
            }
            note.touch();
            s.undo.push(Action::AddStrokes { page, items });
            s.redo.clear();
            s.selection = ids;
            s.selection_page = Some(page);
            s.invalidate_ink();
            true
        };
        if changed {
            self.area.queue_draw();
            emit_view(&self.state);
            notify_changed(&self.state);
        }
        changed
    }

    pub fn delete_selection(&self) -> bool {
        let changed = {
            let mut s = self.state.borrow_mut();
            let Some(page) = s.selection_page else {
                return false;
            };
            let ids = s.selection.clone();
            let Some(note) = &mut s.note else {
                return false;
            };
            let Some(page_data) = note.pages.get_mut(page) else {
                return false;
            };
            let mut items = Vec::new();
            for (index, stroke) in page_data.strokes.iter().enumerate() {
                if ids.contains(&stroke.id) {
                    items.push((index, stroke.clone()));
                }
            }
            if items.is_empty() {
                return false;
            }
            page_data.strokes.retain(|stroke| !ids.contains(&stroke.id));
            note.touch();
            s.undo.push(Action::Erase { page, items });
            s.redo.clear();
            s.clear_selection();
            s.invalidate_ink();
            true
        };
        if changed {
            self.area.queue_draw();
            emit_view(&self.state);
            notify_changed(&self.state);
        }
        changed
    }

    pub fn cut_selection(&self) -> bool {
        if !self.copy_selection() {
            return false;
        }
        self.delete_selection()
    }

    pub fn set_color(&self, color: &str) {
        self.state.borrow_mut().color = color.to_string();
    }

    /// Give the selected strokes a new color. Returns false when nothing changed.
    pub fn recolor_selection(&self, color: &str, highlighter: bool) -> bool {
        let changed = self
            .state
            .borrow_mut()
            .recolor_selection(color, highlighter);
        if changed {
            self.area.queue_draw();
            emit_view(&self.state);
            notify_changed(&self.state);
        }
        changed
    }

    /// Whether the selection holds a stroke in this colour and tool group.
    pub fn selection_has_color(&self, color: &str, highlighter: bool) -> bool {
        self.state.borrow().selection_has_color(color, highlighter)
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

    pub fn set_shape_kind(&self, kind: ShapeKind) {
        self.state.borrow_mut().shape_kind = kind;
    }

    pub fn set_space_down(&self, down: bool) {
        self.state.borrow_mut().space_down = down;
    }

    /// Handle a button that a Bluetooth pen reports as a keyboard key.
    pub fn stylus_key_down(&self, key: u32) {
        self.stylus_aux_button(synthetic_button(key), true);
    }

    pub fn stylus_key_up(&self, key: u32) {
        self.stylus_aux_button(synthetic_button(key), false);
    }

    pub fn stylus_key_held(&self, key: u32) -> bool {
        self.state
            .borrow()
            .stylus_buttons
            .contains(&synthetic_button(key))
    }

    /// Handle a barrel button reported by a separate Bluetooth pen device.
    pub fn stylus_mouse_button(&self, button: u32, pressed: bool) {
        self.stylus_aux_button(button, pressed);
    }

    fn stylus_aux_button(&self, button: u32, pressed: bool) {
        {
            let mut s = self.state.borrow_mut();
            if pressed {
                if !s.stylus_buttons.contains(&button) {
                    s.stylus_buttons.push(button);
                }
            } else {
                s.stylus_buttons.retain(|held| *held != button);
            }
            s.stylus_tool = stylus_tool_for_buttons(&s.stylus_buttons);
        }
        self.area.queue_draw();
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

    /// Last pointer position in canvas coordinates (or the view center).
    pub fn pointer_position(&self) -> (f64, f64) {
        let s = self.state.borrow();
        s.cursor
            .unwrap_or((s.width as f64 / 2.0, s.height as f64 / 2.0))
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
            Action::AddStroke {
                page,
                index,
                stroke,
            } => {
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
            Action::AddStrokes { page, items } => {
                let Some(p) = note.pages.get_mut(*page) else {
                    return false;
                };
                if forward {
                    for (index, stroke) in items {
                        let at = (*index).min(p.strokes.len());
                        p.strokes.insert(at, stroke.clone());
                    }
                } else {
                    for (_, stroke) in items {
                        if let Some(pos) = p.strokes.iter().position(|s| s.id == stroke.id) {
                            p.strokes.remove(pos);
                        } else {
                            return false;
                        }
                    }
                }
                true
            }
            Action::MoveStrokes { page, ids, dx, dy } => {
                let Some(p) = note.pages.get_mut(*page) else {
                    return false;
                };
                let (dx, dy) = if forward { (*dx, *dy) } else { (-*dx, -*dy) };
                let mut found = false;
                for stroke in &mut p.strokes {
                    if !ids.contains(&stroke.id) {
                        continue;
                    }
                    found = true;
                    for point in &mut stroke.points {
                        point.x += dx;
                        point.y += dy;
                    }
                }
                found
            }
            Action::RecolorStrokes {
                page,
                color,
                previous,
            } => {
                let Some(p) = note.pages.get_mut(*page) else {
                    return false;
                };
                let mut found = false;
                for stroke in &mut p.strokes {
                    let Some((_, old)) = previous.iter().find(|(id, _)| *id == stroke.id) else {
                        continue;
                    };
                    found = true;
                    stroke.color = if forward { color.clone() } else { old.clone() };
                }
                found
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

/// Maps currently held stylus barrel buttons to a temporary tool.
/// GDK reports the main barrel button as middle (2) and the second one as
/// secondary (3); a third button is reported as 8. Unknown buttons erase.
fn synthetic_button(key: u32) -> u32 {
    0x8000_0000 | key
}

fn stylus_tool_for_buttons(buttons: &[u32]) -> Option<Tool> {
    if buttons.contains(&2) {
        Some(Tool::Eraser)
    } else if buttons.contains(&3) || buttons.contains(&8) || buttons.len() >= 2 {
        Some(Tool::Hand)
    } else if !buttons.is_empty() {
        Some(Tool::Eraser)
    } else {
        None
    }
}

fn stylus_tool_for_event_state(state: gdk::ModifierType) -> Option<Tool> {
    if state.contains(gdk::ModifierType::BUTTON2_MASK) {
        Some(Tool::Eraser)
    } else if state.contains(gdk::ModifierType::BUTTON3_MASK) {
        Some(Tool::Hand)
    } else {
        None
    }
}

fn select_click(s: &mut State, x: f64, y: f64, additive: bool) {
    let (wx, wy) = s.screen_to_world(x, y);
    let Some(note) = &s.note else {
        return;
    };
    let Some(page) = note.page_at(&s.layout, wx, wy) else {
        if !additive {
            s.selection.clear();
            s.selection_page = None;
        }
        return;
    };
    if s.selection_page != Some(page) {
        s.selection.clear();
        s.selection_page = None;
    }
    let bounds = selection_bounds(note, page, &s.selection);
    if let Some(id) =
        stroke_at(note, page, wx, wy, s.camera.zoom).map(|index| note.pages[page].strokes[index].id)
    {
        if additive {
            if let Some(position) = s.selection.iter().position(|selected| *selected == id) {
                s.selection.remove(position);
            } else {
                s.selection.push(id);
            }
        } else if !s.selection.contains(&id) {
            s.selection = vec![id];
        }
        s.selection_page = if s.selection.is_empty() {
            None
        } else {
            Some(page)
        };
    } else {
        let inside = bounds
            .map(|bounds| expanded_contains(bounds, wx, wy, 6.0 / s.camera.zoom))
            .unwrap_or(false);
        if !additive && !inside {
            s.selection.clear();
            s.selection_page = None;
        }
    }
}

fn pointer_down(s: &mut State, x: f64, y: f64, pressure: f64, additive: bool) {
    if s.note.is_none() || !matches!(&s.interaction, Interaction::None) {
        return;
    }
    let tool = s.active_tool();
    if s.space_down || tool == Tool::Hand {
        s.interaction = Interaction::Pan {
            start: (x, y),
            camera: s.camera,
        };
        return;
    }
    if tool == Tool::Eraser {
        s.clear_selection();
    }
    let (wx, wy) = s.screen_to_world(x, y);
    let Some(note) = &s.note else {
        return;
    };
    let Some(page) = note.page_at(&s.layout, wx, wy) else {
        if tool == Tool::Select {
            s.clear_selection();
        }
        return;
    };
    let (cx, cy) = note.clamp_to_page(&s.layout, page, wx, wy);
    match tool {
        Tool::Pen | Tool::Highlighter => {
            let tool = if tool == Tool::Pen {
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
                shape: None,
            };
            s.interaction = Interaction::Stroke { page, stroke };
        }
        Tool::Eraser => {
            let hit = erase_hits(note, page, wx, wy, s.eraser_size / 2.0 / s.camera.zoom);
            s.interaction = Interaction::Erase { page, hit };
        }
        Tool::Shape => {
            s.interaction = Interaction::Shape {
                page,
                start: (cx, cy),
                current: (cx, cy),
            };
        }
        Tool::Select => {
            if s.selection_page != Some(page) {
                s.selection.clear();
                s.selection_page = None;
            }
            let bounds = selection_bounds(note, page, &s.selection);
            let hit = stroke_at(note, page, wx, wy, s.camera.zoom)
                .map(|index| note.pages[page].strokes[index].id);
            if let Some(id) = hit {
                let was_selected = s.selection.contains(&id);
                if additive {
                    if was_selected {
                        s.selection.retain(|selected| *selected != id);
                        if s.selection.is_empty() {
                            s.selection_page = None;
                        }
                        return;
                    }
                    s.selection.push(id);
                } else if !was_selected {
                    s.selection = vec![id];
                }
                s.selection_page = Some(page);
                let ids = s.selection.clone();
                let originals: Vec<(usize, Stroke)> = note.pages[page]
                    .strokes
                    .iter()
                    .enumerate()
                    .filter(|(_, stroke)| ids.contains(&stroke.id))
                    .map(|(index, stroke)| (index, stroke.clone()))
                    .collect();
                s.interaction = Interaction::MoveSelection {
                    page,
                    start: (wx, wy),
                    delta: (0.0, 0.0),
                    moved: false,
                    ids,
                    originals,
                };
            } else {
                let inside_selection = !additive
                    && bounds
                        .map(|bounds| expanded_contains(bounds, wx, wy, 6.0 / s.camera.zoom))
                        .unwrap_or(false);
                if inside_selection {
                    let ids = s.selection.clone();
                    let originals: Vec<(usize, Stroke)> = note.pages[page]
                        .strokes
                        .iter()
                        .enumerate()
                        .filter(|(_, stroke)| ids.contains(&stroke.id))
                        .map(|(index, stroke)| (index, stroke.clone()))
                        .collect();
                    s.interaction = Interaction::MoveSelection {
                        page,
                        start: (wx, wy),
                        delta: (0.0, 0.0),
                        moved: false,
                        ids,
                        originals,
                    };
                } else {
                    if !additive {
                        s.clear_selection();
                    }
                    s.interaction = Interaction::Select {
                        page,
                        start: (wx, wy),
                        current: (wx, wy),
                        additive,
                    };
                }
            }
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
                let new_hits = erase_hits(note, *page, wx, wy, s.eraser_size / 2.0 / s.camera.zoom);
                for index in new_hits {
                    if !hit.contains(&index) {
                        hit.push(index);
                    }
                }
            }
        }
        Interaction::Select { current, .. } => {
            *current = (wx, wy);
        }
        Interaction::Shape { page, current, .. } => {
            let Some(note) = &s.note else {
                return;
            };
            *current = note.clamp_to_page(&s.layout, *page, wx, wy);
            s.cache_dirty = true;
        }
        Interaction::MoveSelection {
            page,
            start,
            delta,
            moved,
            ids: _,
            originals,
        } => {
            let mut dx = wx - start.0;
            let mut dy = wy - start.1;
            let threshold = 2.5 / s.camera.zoom;
            if !*moved && dx * dx + dy * dy < threshold * threshold {
                return;
            }
            *moved = true;
            if let Some(rect) = s.layout.get(*page).copied() {
                if let Some(bounds) = originals
                    .iter()
                    .filter_map(|(_, stroke)| stroke_bounds(stroke))
                    .reduce(union_rect)
                {
                    (dx, dy) = clamp_translation(bounds, rect, dx, dy);
                }
            }
            *delta = (dx, dy);
            if let Some(note) = &mut s.note {
                if let Some(page_data) = note.pages.get_mut(*page) {
                    for (index, original) in originals.iter() {
                        if let Some(stroke) = page_data.strokes.get_mut(*index) {
                            stroke.points = original
                                .points
                                .iter()
                                .map(|point| Point {
                                    x: point.x + dx,
                                    y: point.y + dy,
                                    p: point.p,
                                })
                                .collect();
                        }
                    }
                }
            }
            s.path_cache.clear();
            s.cache_dirty = true;
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
                stroke.points.push(Point {
                    x: p.x + 0.01,
                    y: p.y + 0.01,
                    p: p.p,
                });
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
                let new_hits = erase_hits(note, page, wx, wy, s.eraser_size / 2.0 / s.camera.zoom);
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
        Interaction::Shape {
            page,
            start,
            current,
        } => {
            let scale = s.camera.zoom.max(0.01);
            let dx = current.0 - start.0;
            let dy = current.1 - start.1;
            if (dx * dx + dy * dy).sqrt() * scale < 3.0 {
                s.cache_dirty = true;
                return false;
            }
            let stroke = Stroke {
                id: next_stroke_id(),
                tool: StrokeTool::Pen,
                color: s.color.clone(),
                width: s.pen_size,
                points: vec![
                    Point {
                        x: start.0,
                        y: start.1,
                        p: 0.5,
                    },
                    Point {
                        x: current.0,
                        y: current.1,
                        p: 0.5,
                    },
                ],
                shape: Some(s.shape_kind),
            };
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
        Interaction::Select {
            page,
            start,
            current,
            additive,
        } => {
            let rect = rect_from_points(start, current);
            let threshold = 3.0 / s.camera.zoom;
            if rect.w >= threshold || rect.h >= threshold {
                let ids: Vec<u64> = {
                    let Some(note) = &s.note else {
                        return false;
                    };
                    let Some(page_data) = note.pages.get(page) else {
                        return false;
                    };
                    page_data
                        .strokes
                        .iter()
                        .filter(|stroke| stroke_intersects_rect(stroke, rect))
                        .map(|stroke| stroke.id)
                        .collect()
                };
                if additive {
                    for id in ids {
                        if !s.selection.contains(&id) {
                            s.selection.push(id);
                        }
                    }
                } else {
                    s.selection = ids;
                }
                s.selection_page = if s.selection.is_empty() {
                    None
                } else {
                    Some(page)
                };
            }
            false
        }
        Interaction::MoveSelection {
            page,
            moved,
            delta,
            ids,
            ..
        } => {
            if !moved {
                return false;
            }
            if let Some(note) = &mut s.note {
                note.touch();
            }
            s.undo.push(Action::MoveStrokes {
                page,
                ids,
                dx: delta.0,
                dy: delta.1,
            });
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
    if let Some(kind) = stroke.shape {
        if shape_contains(kind, &stroke.points, x, y) {
            return true;
        }
        return shape_segments(kind, &stroke.points)
            .iter()
            .any(|seg| segment_dist2(seg.0 .0, seg.0 .1, seg.1 .0, seg.1 .1, x, y) <= t2);
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

fn rect_from_points(a: (f64, f64), b: (f64, f64)) -> Rect {
    Rect {
        x: a.0.min(b.0),
        y: a.1.min(b.1),
        w: (a.0 - b.0).abs(),
        h: (a.1 - b.1).abs(),
    }
}

fn stroke_bounds(stroke: &Stroke) -> Option<Rect> {
    let first = stroke.points.first()?;
    let margin = stroke.width.max(0.4) * 0.5;
    let mut min_x = first.x - margin;
    let mut max_x = first.x + margin;
    let mut min_y = first.y - margin;
    let mut max_y = first.y + margin;
    for point in &stroke.points[1..] {
        min_x = min_x.min(point.x - margin);
        max_x = max_x.max(point.x + margin);
        min_y = min_y.min(point.y - margin);
        max_y = max_y.max(point.y + margin);
    }
    Some(Rect {
        x: min_x,
        y: min_y,
        w: max_x - min_x,
        h: max_y - min_y,
    })
}

fn stroke_intersects_rect(stroke: &Stroke, rect: Rect) -> bool {
    let Some(bounds) = stroke_bounds(stroke) else {
        return false;
    };
    if !bounds.intersects(rect.x, rect.y, rect.w, rect.h) {
        return false;
    }
    if let Some(kind) = stroke.shape {
        // A marquee inside the shape, or around the whole shape, still selects it.
        if shape_contains(
            kind,
            &stroke.points,
            rect.x + rect.w * 0.5,
            rect.y + rect.h * 0.5,
        ) {
            return true;
        }
        if rect.x <= bounds.x
            && rect.y <= bounds.y
            && rect.x + rect.w >= bounds.x + bounds.w
            && rect.y + rect.h >= bounds.y + bounds.h
        {
            return true;
        }
        return shape_segments(kind, &stroke.points)
            .iter()
            .any(|seg| segment_rect_intersects(seg.0 .0, seg.0 .1, seg.1 .0, seg.1 .1, rect));
    }
    if stroke.points.len() == 1 {
        let p = &stroke.points[0];
        return rect.contains(p.x, p.y);
    }
    for pair in stroke.points.windows(2) {
        if segment_rect_intersects(pair[0].x, pair[0].y, pair[1].x, pair[1].y, rect) {
            return true;
        }
    }
    false
}

/// Liang-Barsky segment/rectangle clipping test.
fn segment_rect_intersects(x1: f64, y1: f64, x2: f64, y2: f64, rect: Rect) -> bool {
    if rect.contains(x1, y1) || rect.contains(x2, y2) {
        return true;
    }
    let dx = x2 - x1;
    let dy = y2 - y1;
    let p = [-dx, dx, -dy, dy];
    let q = [
        x1 - rect.x,
        rect.x + rect.w - x1,
        y1 - rect.y,
        rect.y + rect.h - y1,
    ];
    let mut t0 = 0.0_f64;
    let mut t1 = 1.0_f64;
    for i in 0..4 {
        if p[i].abs() < 1e-12 {
            if q[i] < 0.0 {
                return false;
            }
        } else {
            let t = q[i] / p[i];
            if p[i] < 0.0 {
                t0 = t0.max(t);
            } else {
                t1 = t1.min(t);
            }
            if t0 > t1 {
                return false;
            }
        }
    }
    true
}

fn stroke_at(note: &Note, page: usize, x: f64, y: f64, zoom: f64) -> Option<usize> {
    let page_data = note.pages.get(page)?;
    let threshold = 7.0 / zoom;
    page_data
        .strokes
        .iter()
        .enumerate()
        .rev()
        .find(|(_, stroke)| stroke_hit(stroke, x, y, threshold + stroke.width * 0.5))
        .map(|(index, _)| index)
}

fn union_rect(a: Rect, b: Rect) -> Rect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    let right = (a.x + a.w).max(b.x + b.w);
    let bottom = (a.y + a.h).max(b.y + b.h);
    Rect {
        x,
        y,
        w: right - x,
        h: bottom - y,
    }
}

fn clipboard_bounds(strokes: &[Stroke]) -> Option<Rect> {
    strokes.iter().filter_map(stroke_bounds).reduce(union_rect)
}

fn selection_bounds(note: &Note, page: usize, ids: &[u64]) -> Option<Rect> {
    note.pages
        .get(page)?
        .strokes
        .iter()
        .filter(|stroke| ids.contains(&stroke.id))
        .filter_map(stroke_bounds)
        .reduce(union_rect)
}

fn expanded_contains(bounds: Rect, x: f64, y: f64, pad: f64) -> bool {
    x >= bounds.x - pad
        && x <= bounds.x + bounds.w + pad
        && y >= bounds.y - pad
        && y <= bounds.y + bounds.h + pad
}

fn clamp_translation(bounds: Rect, target: Rect, dx: f64, dy: f64) -> (f64, f64) {
    let min_dx = target.x + 1.0 - bounds.x;
    let max_dx = target.x + target.w - 1.0 - (bounds.x + bounds.w);
    let min_dy = target.y + 1.0 - bounds.y;
    let max_dy = target.y + target.h - 1.0 - (bounds.y + bounds.h);
    let clamp_axis = |value: f64, min: f64, max: f64| {
        if min > max {
            (min + max) * 0.5
        } else {
            value.clamp(min, max)
        }
    };
    (
        clamp_axis(dx, min_dx, max_dx),
        clamp_axis(dy, min_dy, max_dy),
    )
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
        let gradient =
            cairo::RadialGradient::new(w * 0.5, h * 0.42, 0.0, w * 0.5, h * 0.42, w.max(h) * 0.85);
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
        ctx.set_source_rgba(0.0, 0.0, 0.0, if dark { 0.42 } else { 0.075 });
        rounded_rect(ctx, rect.x + 1.0, rect.y + 6.0, rect.w, rect.h, radius);
        let _ = ctx.fill();
        ctx.set_source_rgba(0.0, 0.0, 0.0, if dark { 0.18 } else { 0.035 });
        rounded_rect(
            ctx,
            rect.x - 4.0,
            rect.y + 10.0,
            rect.w + 8.0,
            rect.h,
            radius + 6.0,
        );
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
        rounded_rect(
            ctx,
            rect.x + 0.5,
            rect.y + 0.5,
            rect.w - 1.0,
            rect.h - 1.0,
            radius,
        );
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
        if self.pdf_scale <= 0.0 || target > self.pdf_scale * 1.25 || target < self.pdf_scale * 0.42
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
            Interaction::Stroke { page, stroke } => Some((*page, LiveItem::Stroke(stroke.clone()))),
            Interaction::Shape {
                page,
                start,
                current,
            } => {
                let stroke = Stroke {
                    id: 0,
                    tool: StrokeTool::Pen,
                    color: self.color.clone(),
                    width: self.pen_size,
                    points: vec![
                        Point {
                            x: start.0,
                            y: start.1,
                            p: 0.5,
                        },
                        Point {
                            x: current.0,
                            y: current.1,
                            p: 0.5,
                        },
                    ],
                    shape: Some(self.shape_kind),
                };
                Some((*page, LiveItem::Stroke(stroke)))
            }
            Interaction::Erase { page, hit } => Some((*page, LiveItem::Erase(hit.clone()))),
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
            let clip = if infinite {
                None
            } else {
                self.layout.get(page).copied()
            };
            if let Some(rect) = clip {
                ctx.save();
                rounded_rect(ctx, rect.x, rect.y, rect.w, rect.h, 7.0);
                ctx.clip();
                ctx.new_path();
            }
            match item {
                LiveItem::Stroke(stroke) => {
                    self.scratch_ctx.new_path();
                    build_live_stroke_path(&self.scratch_ctx, &stroke);
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
                                        ctx.set_line_width((stroke.width + 0.5).max(0.4));
                                        ctx.set_line_cap(cairo::LineCap::Round);
                                        ctx.set_line_join(cairo::LineJoin::Round);
                                        ctx.set_source_rgba(0.58, 0.60, 0.63, 1.0);
                                        let _ = ctx.stroke();
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

        let mut selected_bounds: Option<Rect> = None;
        if let Some(page) = self.selection_page {
            let indices = self.selection_indices(page);
            if !indices.is_empty() {
                let infinite = self
                    .note
                    .as_ref()
                    .map(|n| n.kind == NoteKind::Infinite)
                    .unwrap_or(false);
                let clip = if infinite {
                    None
                } else {
                    self.layout.get(page).copied()
                };
                if let Some(rect) = clip {
                    ctx.save();
                    rounded_rect(ctx, rect.x, rect.y, rect.w, rect.h, 7.0);
                    ctx.clip();
                    ctx.new_path();
                }
                for index in &indices {
                    self.ensure_path(page, *index);
                }
                if let Some(note) = &self.note {
                    if let Some(page_data) = note.pages.get(page) {
                        let mut bounds: Option<Rect> = None;
                        for index in &indices {
                            if let Some(stroke) = page_data.strokes.get(*index) {
                                if let Some(stroke_bounds) = stroke_bounds(stroke) {
                                    bounds = Some(match bounds {
                                        None => stroke_bounds,
                                        Some(current) => union_rect(current, stroke_bounds),
                                    });
                                }
                                if let Some(path) = self.path_cache.get(&(page, *index)) {
                                    ctx.new_path();
                                    ctx.append_path(path);
                                    ctx.set_line_width((stroke.width + 5.0 / zoom).max(1.0));
                                    ctx.set_line_cap(cairo::LineCap::Round);
                                    ctx.set_line_join(cairo::LineJoin::Round);
                                    ctx.set_source_rgba(0.20, 0.48, 0.95, 0.28);
                                    let _ = ctx.stroke();
                                }
                            }
                        }
                        selected_bounds = bounds;
                    }
                }
                if clip.is_some() {
                    ctx.restore();
                }
            }
        }
        ctx.restore();

        if let Some(bounds) = selected_bounds {
            let rect = Rect {
                x: bounds.x * zoom + tx - 4.0,
                y: bounds.y * zoom + ty - 4.0,
                w: bounds.w * zoom + 8.0,
                h: bounds.h * zoom + 8.0,
            };
            ctx.set_source_rgba(0.20, 0.48, 0.95, 0.07);
            rounded_rect(ctx, rect.x, rect.y, rect.w, rect.h, 5.0);
            let _ = ctx.fill();
            ctx.set_source_rgba(0.20, 0.48, 0.95, 0.9);
            ctx.set_line_width(1.2);
            ctx.set_dash(&[5.0, 4.0], 0.0);
            rounded_rect(ctx, rect.x, rect.y, rect.w, rect.h, 5.0);
            let _ = ctx.stroke();
            ctx.set_dash(&[], 0.0);
        }

        if let Interaction::Select { start, current, .. } = &self.interaction {
            let rect = rect_from_points(*start, *current);
            let sx = rect.x * zoom + tx;
            let sy = rect.y * zoom + ty;
            let sw = rect.w * zoom;
            let sh = rect.h * zoom;
            ctx.set_source_rgba(0.20, 0.48, 0.95, 0.09);
            ctx.rectangle(sx, sy, sw, sh);
            let _ = ctx.fill();
            ctx.set_source_rgba(0.20, 0.48, 0.95, 0.9);
            ctx.set_line_width(1.2);
            ctx.set_dash(&[5.0, 4.0], 0.0);
            ctx.rectangle(sx, sy, sw, sh);
            let _ = ctx.stroke();
            ctx.set_dash(&[], 0.0);
        }

        // Cursor.
        if let Some((x, y)) = self.cursor {
            let tool = self.active_tool();
            let hand = self.space_down || tool == Tool::Hand;
            let radius = if hand {
                7.0
            } else {
                match tool {
                    Tool::Pen => (self.pen_size * zoom * 0.5).max(2.0),
                    Tool::Highlighter => (self.highlighter_size * zoom * 0.5).max(4.0),
                    Tool::Eraser => self.eraser_size * 0.5,
                    Tool::Shape => (self.pen_size * zoom * 0.5).max(2.0),
                    Tool::Select => 4.0,
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

/// Normalized bounding box of a shape gesture: (left, top, right, bottom).
fn shape_box(points: &[Point]) -> Option<(f64, f64, f64, f64)> {
    let a = points.first()?;
    let b = points.get(1).unwrap_or(a);
    Some((a.x.min(b.x), a.y.min(b.y), a.x.max(b.x), a.y.max(b.y)))
}

fn append_shape_path(ctx: &Context, kind: ShapeKind, points: &[Point]) {
    let Some((x0, y0, x1, y1)) = shape_box(points) else {
        return;
    };
    match kind {
        ShapeKind::Line => {
            let a = points[0];
            let b = points.get(1).copied().unwrap_or(a);
            ctx.move_to(a.x, a.y);
            ctx.line_to(b.x, b.y);
        }
        ShapeKind::Rect => {
            ctx.move_to(x0, y0);
            ctx.line_to(x1, y0);
            ctx.line_to(x1, y1);
            ctx.line_to(x0, y1);
            ctx.close_path();
        }
        ShapeKind::Ellipse => {
            let cx = (x0 + x1) * 0.5;
            let cy = (y0 + y1) * 0.5;
            let rx = ((x1 - x0) * 0.5).max(0.01);
            let ry = ((y1 - y0) * 0.5).max(0.01);
            let _ = ctx.save();
            let _ = ctx.translate(cx, cy);
            let _ = ctx.scale(rx, ry);
            ctx.arc(0.0, 0.0, 1.0, 0.0, TAU);
            let _ = ctx.restore();
            ctx.close_path();
        }
        ShapeKind::Triangle => {
            let cx = (x0 + x1) * 0.5;
            ctx.move_to(cx, y0);
            ctx.line_to(x1, y1);
            ctx.line_to(x0, y1);
            ctx.close_path();
        }
    }
}

/// Outline of a shape as a closed polyline, used for hit testing.
fn shape_polygon(kind: ShapeKind, points: &[Point]) -> Vec<(f64, f64)> {
    let Some((x0, y0, x1, y1)) = shape_box(points) else {
        return Vec::new();
    };
    match kind {
        ShapeKind::Line => {
            let a = points[0];
            let b = points.get(1).copied().unwrap_or(a);
            vec![(a.x, a.y), (b.x, b.y)]
        }
        ShapeKind::Rect => vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1), (x0, y0)],
        ShapeKind::Ellipse => {
            let cx = (x0 + x1) * 0.5;
            let cy = (y0 + y1) * 0.5;
            let rx = (x1 - x0) * 0.5;
            let ry = (y1 - y0) * 0.5;
            const STEPS: usize = 64;
            (0..=STEPS)
                .map(|step| {
                    let angle = TAU * step as f64 / STEPS as f64;
                    (cx + rx * angle.cos(), cy + ry * angle.sin())
                })
                .collect()
        }
        ShapeKind::Triangle => {
            let cx = (x0 + x1) * 0.5;
            vec![(cx, y0), (x1, y1), (x0, y1), (cx, y0)]
        }
    }
}

fn shape_segments(kind: ShapeKind, points: &[Point]) -> Vec<((f64, f64), (f64, f64))> {
    shape_polygon(kind, points)
        .windows(2)
        .map(|pair| (pair[0], pair[1]))
        .collect()
}

/// Whether a point falls inside the area a shape encloses (tapping a rectangle
/// selects it, tapping a line does not).
fn shape_contains(kind: ShapeKind, points: &[Point], x: f64, y: f64) -> bool {
    let Some((x0, y0, x1, y1)) = shape_box(points) else {
        return false;
    };
    match kind {
        ShapeKind::Line => false,
        ShapeKind::Rect => x >= x0 && x <= x1 && y >= y0 && y <= y1,
        ShapeKind::Ellipse => {
            let cx = (x0 + x1) * 0.5;
            let cy = (y0 + y1) * 0.5;
            let rx = (x1 - x0) * 0.5;
            let ry = (y1 - y0) * 0.5;
            if rx < 1e-6 || ry < 1e-6 {
                return false;
            }
            let nx = (x - cx) / rx;
            let ny = (y - cy) / ry;
            nx * nx + ny * ny <= 1.0
        }
        ShapeKind::Triangle => polygon_contains(&shape_polygon(kind, points), x, y),
    }
}

fn polygon_contains(polygon: &[(f64, f64)], x: f64, y: f64) -> bool {
    let mut inside = false;
    for pair in polygon.windows(2) {
        let (x1, y1) = pair[0];
        let (x2, y2) = pair[1];
        if (y1 > y) != (y2 > y) {
            let t = (y - y1) / (y2 - y1);
            if x < x1 + t * (x2 - x1) {
                inside = !inside;
            }
        }
    }
    inside
}

/// Cheap path for the in-progress stroke: one cubic per input point instead of
/// resampling the whole line at sub-pixel intervals on every motion event.
/// The committed stroke is rebuilt at full quality when the pen is lifted.
fn build_live_stroke_path(ctx: &Context, stroke: &Stroke) {
    if let Some(kind) = stroke.shape {
        append_shape_path(ctx, kind, &stroke.points);
        return;
    }
    let points = smooth_points(&stroke.points);
    if points.is_empty() {
        return;
    }
    if points.len() == 1 {
        let (x, y) = (points[0].x, points[0].y);
        ctx.move_to(x, y);
        ctx.line_to(x + 0.01, y);
        return;
    }
    let xy: Vec<(f64, f64)> = points.iter().map(|p| (p.x, p.y)).collect();
    append_smooth_polyline(ctx, &xy);
}

fn build_stroke_path(ctx: &Context, stroke: &Stroke) {
    if let Some(kind) = stroke.shape {
        append_shape_path(ctx, kind, &stroke.points);
        return;
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with_stroke() -> State {
        let mut state = State::new();
        let mut note = Note::new(NoteKind::Infinite, "test");
        note.pages[0].strokes.push(Stroke {
            id: 1,
            tool: StrokeTool::Pen,
            color: "#000000".into(),
            width: 4.0,
            points: vec![
                Point {
                    x: 10.0,
                    y: 10.0,
                    p: 0.5,
                },
                Point {
                    x: 20.0,
                    y: 20.0,
                    p: 0.5,
                },
            ],
            shape: None,
        });
        state.layout = note.layout();
        state.note = Some(note);
        state.tool = Tool::Select;
        state
    }

    #[test]
    fn tap_selects_a_stroke() {
        let mut state = state_with_stroke();
        pointer_down(&mut state, 15.0, 15.0, 0.5, false);
        pointer_up(&mut state, 15.0, 15.0, 0.5);
        assert_eq!(state.selection, vec![1]);
        assert_eq!(state.selection_page, Some(0));
    }

    #[test]
    fn marquee_selects_intersecting_strokes() {
        let mut state = state_with_stroke();
        pointer_down(&mut state, 0.0, 0.0, 0.5, false);
        pointer_move(&mut state, 30.0, 30.0, 0.5);
        pointer_up(&mut state, 30.0, 30.0, 0.5);
        assert_eq!(state.selection, vec![1]);
    }

    #[test]
    fn dragging_selection_moves_and_undoes() {
        let mut state = state_with_stroke();
        pointer_down(&mut state, 15.0, 15.0, 0.5, false);
        pointer_move(&mut state, 35.0, 25.0, 0.5);
        assert!(pointer_up(&mut state, 35.0, 25.0, 0.5));
        let stroke = &state.note.as_ref().unwrap().pages[0].strokes[0];
        assert!((stroke.points[0].x - 30.0).abs() < 1e-9);
        assert!((stroke.points[0].y - 20.0).abs() < 1e-9);
        let action = state.undo.pop().unwrap();
        assert!(state.apply_action(&action, false));
        let stroke = &state.note.as_ref().unwrap().pages[0].strokes[0];
        assert!((stroke.points[0].x - 10.0).abs() < 1e-9);
        assert!((stroke.points[0].y - 10.0).abs() < 1e-9);
    }

    #[test]
    fn clicking_a_selected_stroke_keeps_the_group() {
        let mut state = state_with_stroke();
        state.selection = vec![1, 2];
        state.selection_page = Some(0);
        select_click(&mut state, 15.0, 15.0, false);
        assert_eq!(state.selection, vec![1, 2]);
    }

    #[test]
    fn dragging_inside_selection_bounds_moves_the_whole_group() {
        let mut state = state_with_stroke();
        state.note.as_mut().unwrap().pages[0].strokes.push(Stroke {
            id: 2,
            tool: StrokeTool::Pen,
            color: "#000000".into(),
            width: 4.0,
            points: vec![
                Point {
                    x: 50.0,
                    y: 50.0,
                    p: 0.5,
                },
                Point {
                    x: 60.0,
                    y: 60.0,
                    p: 0.5,
                },
            ],
            shape: None,
        });
        state.selection = vec![1, 2];
        state.selection_page = Some(0);
        // Empty space between the two strokes, but inside the group bounds.
        pointer_down(&mut state, 40.0, 40.0, 0.5, false);
        pointer_move(&mut state, 50.0, 50.0, 0.5);
        assert!(pointer_up(&mut state, 50.0, 50.0, 0.5));
        let strokes = &state.note.as_ref().unwrap().pages[0].strokes;
        assert!((strokes[0].points[0].x - 20.0).abs() < 1e-9);
        assert!((strokes[1].points[0].x - 60.0).abs() < 1e-9);
    }

    #[test]
    fn recolor_selection_changes_and_undoes() {
        let mut state = state_with_stroke();
        state.selection = vec![1];
        state.selection_page = Some(0);
        assert!(state.recolor_selection("#ff0000", false));
        let stroke = &state.note.as_ref().unwrap().pages[0].strokes[0];
        assert_eq!(stroke.color, "#ff0000");
        // Same color again is a no-op, so undo never records empty steps.
        assert!(!state.recolor_selection("#ff0000", false));

        let action = state.undo.pop().unwrap();
        assert!(state.apply_action(&action, false));
        let stroke = &state.note.as_ref().unwrap().pages[0].strokes[0];
        assert_eq!(stroke.color, "#000000");
        assert!(state.apply_action(&action, true));
        let stroke = &state.note.as_ref().unwrap().pages[0].strokes[0];
        assert_eq!(stroke.color, "#ff0000");
    }

    #[test]
    fn recolor_only_touches_the_selected_tool_group() {
        let mut state = state_with_stroke();
        state.selection = vec![1];
        state.selection_page = Some(0);
        assert!(!state.recolor_selection("#ffe066", true));
        assert_eq!(
            state.note.as_ref().unwrap().pages[0].strokes[0].color,
            "#000000"
        );
        assert!(state.recolor_selection("#2f6fed", false));
    }

    #[test]
    fn selection_reports_only_the_colours_it_uses() {
        let mut state = state_with_stroke();
        state.selection = vec![1];
        state.selection_page = Some(0);
        assert!(state.selection_has_color("#000000", false));
        assert!(!state.selection_has_color("#000000", true));
        assert!(!state.selection_has_color("#2f6fed", false));

        state.clear_selection();
        assert!(!state.selection_has_color("#000000", false));
    }

    #[test]
    fn segment_rect_intersection_handles_crossing() {
        let rect = Rect {
            x: 5.0,
            y: 5.0,
            w: 10.0,
            h: 10.0,
        };
        assert!(segment_rect_intersects(0.0, 0.0, 20.0, 20.0, rect));
        assert!(!segment_rect_intersects(0.0, 20.0, 20.0, 20.0, rect));
    }

    fn state_for_shapes() -> State {
        let mut state = State::new();
        let note = Note::new(NoteKind::Paged, "shapes");
        state.layout = note.layout();
        state.note = Some(note);
        state.tool = Tool::Shape;
        state
    }

    fn draw_shape(state: &mut State, kind: ShapeKind, from: (f64, f64), to: (f64, f64)) -> bool {
        state.shape_kind = kind;
        pointer_down(state, from.0, from.1, 0.5, false);
        pointer_move(state, to.0, to.1, 0.5);
        pointer_up(state, to.0, to.1, 0.5)
    }

    #[test]
    fn shape_tool_draws_and_undoes_each_shape() {
        for kind in ShapeKind::ALL {
            let mut state = state_for_shapes();
            assert!(draw_shape(&mut state, kind, (100.0, 100.0), (240.0, 180.0)));
            let color = state.color.clone();
            let strokes = &state.note.as_ref().unwrap().pages[0].strokes;
            assert_eq!(strokes.len(), 1, "{kind:?}");
            assert_eq!(strokes[0].shape, Some(kind));
            assert_eq!(strokes[0].points.len(), 2);
            assert_eq!(strokes[0].color, color);

            let action = state.undo.pop().unwrap();
            assert!(state.apply_action(&action, false));
            assert!(state.note.as_ref().unwrap().pages[0].strokes.is_empty());
        }
    }

    #[test]
    fn tiny_shape_gestures_are_ignored() {
        let mut state = state_for_shapes();
        assert!(!draw_shape(
            &mut state,
            ShapeKind::Line,
            (100.0, 100.0),
            (100.5, 100.5)
        ));
        assert!(state.note.as_ref().unwrap().pages[0].strokes.is_empty());
        assert!(state.undo.is_empty());
    }

    #[test]
    fn shapes_can_be_hit_tested_and_selected() {
        let mut state = state_for_shapes();
        assert!(draw_shape(
            &mut state,
            ShapeKind::Rect,
            (10.0, 10.0),
            (110.0, 60.0)
        ));
        assert!(draw_shape(
            &mut state,
            ShapeKind::Ellipse,
            (200.0, 10.0),
            (280.0, 70.0)
        ));
        let strokes = &state.note.as_ref().unwrap().pages[0].strokes;
        let (rect, ellipse) = (&strokes[0], &strokes[1]);
        let rect_id = rect.id;

        // The outline is a hit, and so is the enclosed area.
        assert!(stroke_hit(rect, 10.0, 35.0, 2.0));
        assert!(stroke_hit(rect, 60.0, 35.0, 2.0));
        assert!(!stroke_hit(rect, 160.0, 35.0, 2.0));
        assert!(stroke_hit(ellipse, 240.0, 40.0, 2.0));
        assert!(!stroke_hit(ellipse, 202.0, 12.0, 2.0));

        let marquee = Rect {
            x: 0.0,
            y: 0.0,
            w: 130.0,
            h: 80.0,
        };
        assert!(stroke_intersects_rect(rect, marquee));
        assert!(!stroke_intersects_rect(ellipse, marquee));

        state.tool = Tool::Select;
        select_click(&mut state, 60.0, 35.0, false);
        assert_eq!(state.selection, vec![rect_id]);
    }

    /// Draws one shape outline into a 100x100 surface, so the rendered pixels
    /// can be probed without a window.
    fn rasterize(kind: ShapeKind, from: (f64, f64), to: (f64, f64)) -> ImageSurface {
        let surface = ImageSurface::create(Format::A8, 100, 100).unwrap();
        let ctx = Context::new(&surface).unwrap();
        ctx.set_line_width(1.0);
        let points = [
            Point {
                x: from.0,
                y: from.1,
                p: 0.5,
            },
            Point {
                x: to.0,
                y: to.1,
                p: 0.5,
            },
        ];
        append_shape_path(&ctx, kind, &points);
        ctx.stroke().unwrap();
        drop(ctx);
        surface.flush();
        surface
    }

    /// Whether any ink landed within two pixels of the point.
    fn ink_near(surface: &mut ImageSurface, x: usize, y: usize) -> bool {
        let stride = surface.stride() as usize;
        let data = surface.data().unwrap();
        let (x0, x1) = (x.saturating_sub(2), (x + 2).min(99));
        let (y0, y1) = (y.saturating_sub(2), (y + 2).min(99));
        (y0..=y1).any(|row| (x0..=x1).any(|col| data[row * stride + col] > 96))
    }

    #[test]
    fn each_shape_kind_is_drawn_where_expected() {
        let mut line = rasterize(ShapeKind::Line, (10.0, 10.0), (90.0, 90.0));
        assert!(ink_near(&mut line, 50, 50));
        assert!(ink_near(&mut line, 90, 90));
        assert!(!ink_near(&mut line, 50, 20));

        let mut rect = rasterize(ShapeKind::Rect, (10.0, 10.0), (90.0, 90.0));
        for (x, y) in [(50, 10), (90, 50), (50, 90), (10, 50)] {
            assert!(ink_near(&mut rect, x, y), "rect edge at {x},{y}");
        }
        assert!(!ink_near(&mut rect, 50, 50));

        let mut ellipse = rasterize(ShapeKind::Ellipse, (10.0, 10.0), (90.0, 90.0));
        for (x, y) in [(50, 10), (90, 50), (50, 90), (10, 50)] {
            assert!(ink_near(&mut ellipse, x, y), "ellipse rim at {x},{y}");
        }
        // The corners of the bounding box stay empty, unlike a rectangle.
        assert!(!ink_near(&mut ellipse, 12, 12));
        assert!(!ink_near(&mut ellipse, 50, 50));

        let mut triangle = rasterize(ShapeKind::Triangle, (10.0, 10.0), (90.0, 90.0));
        for (x, y) in [(50, 10), (90, 90), (10, 90), (30, 50), (70, 50)] {
            assert!(ink_near(&mut triangle, x, y), "triangle edge at {x},{y}");
        }
        assert!(!ink_near(&mut triangle, 50, 75));
        assert!(!ink_near(&mut triangle, 12, 12));
    }
}
