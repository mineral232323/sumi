use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::f64::consts::{PI, TAU};
use std::rc::Rc;
use std::time::Duration;

use gtk::gdk;
use gtk::glib;
use gtk::glib::translate::IntoGlib;
use gtk::prelude::*;
use gtk::{pango, Orientation};
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::canvas::{Canvas, Tool, ViewInfo};
use crate::model::{now_secs, Note, NoteKind, NoteMeta};
use crate::pdf::{self, PdfDoc};
use crate::store::Store;

const PEN_COLORS: [(&str, &str); 6] = [
    ("ink", "#23262b"),
    ("blue", "#2f6fed"),
    ("red", "#e5484d"),
    ("green", "#2e9e6b"),
    ("amber", "#f59e0b"),
    ("violet", "#8b5cf6"),
];

const HIGHLIGHT_COLORS: [(&str, &str); 5] = [
    ("yellow", "#ffe066"),
    ("green", "#a7f3b5"),
    ("blue", "#9bd7ff"),
    ("pink", "#ffc2e2"),
    ("orange", "#ffd3a5"),
];

struct Toolbar {
    root: gtk::Box,
    pen: gtk::ToggleButton,
    highlighter: gtk::ToggleButton,
    eraser: gtk::ToggleButton,
    hand: gtk::ToggleButton,
    palette: gtk::Box,
    pen_palette: gtk::Box,
    highlight_palette: gtk::Box,
    size: gtk::Scale,
    size_value: gtk::Label,
    swatches: Vec<(gtk::Button, String, bool)>,
}

impl Toolbar {
    fn button(&self, tool: Tool) -> &gtk::ToggleButton {
        match tool {
            Tool::Pen => &self.pen,
            Tool::Highlighter => &self.highlighter,
            Tool::Eraser => &self.eraser,
            Tool::Hand => &self.hand,
        }
    }
}

#[derive(Clone, Copy)]
enum WheelAction {
    Tool(Tool),
    Undo,
    Redo,
}

struct WheelButton {
    button: gtk::Button,
    action: WheelAction,
}

const WHEEL_SIZE: i32 = 268;
const WHEEL_RADIUS: f64 = 92.0;
const WHEEL_BUTTON: i32 = 50;
const WHEEL_CENTER: i32 = 42;

struct ToolWheel {
    root: gtk::Fixed,
    circle: gtk::Overlay,
    buttons: Vec<WheelButton>,
    center: gtk::Button,
    position: Rc<Cell<(f64, f64)>>,
}

impl ToolWheel {
    fn new() -> Self {
        let root = gtk::Fixed::new();
        root.set_hexpand(true);
        root.set_vexpand(true);
        root.set_halign(gtk::Align::Fill);
        root.set_valign(gtk::Align::Fill);
        root.set_can_target(true);
        root.set_visible(false);

        let circle = gtk::Overlay::new();
        circle.add_css_class("sumi-wheel");
        circle.set_size_request(WHEEL_SIZE, WHEEL_SIZE);
        circle.set_can_target(true);

        let layout = gtk::Fixed::new();
        circle.set_child(Some(&layout));

        // Add new wheel entries here. The layout distributes them evenly.
        let entries = [
            ("sumi-pen-symbolic", "ペン", WheelAction::Tool(Tool::Pen)),
            (
                "sumi-highlighter-symbolic",
                "マーカー",
                WheelAction::Tool(Tool::Highlighter),
            ),
            ("sumi-eraser-symbolic", "消しゴム", WheelAction::Tool(Tool::Eraser)),
            ("sumi-hand-symbolic", "手のひら", WheelAction::Tool(Tool::Hand)),
            ("edit-undo-symbolic", "元に戻す", WheelAction::Undo),
            ("edit-redo-symbolic", "やり直す", WheelAction::Redo),
        ];

        let count = entries.len() as f64;
        let center = WHEEL_SIZE as f64 / 2.0;
        let mut buttons = Vec::new();
        for (index, (icon, label, action)) in entries.into_iter().enumerate() {
            let angle = -PI / 2.0 + TAU * index as f64 / count;
            let button = gtk::Button::from_icon_name(icon);
            button.add_css_class("wheel-item");
            button.set_size_request(WHEEL_BUTTON, WHEEL_BUTTON);
            button.set_tooltip_text(Some(label));
            button.set_focus_on_click(false);
            layout.put(
                &button,
                center + WHEEL_RADIUS * angle.cos() - WHEEL_BUTTON as f64 / 2.0,
                center + WHEEL_RADIUS * angle.sin() - WHEEL_BUTTON as f64 / 2.0,
            );
            buttons.push(WheelButton { button, action });
        }

        let center_button = gtk::Button::from_icon_name("window-close-symbolic");
        center_button.add_css_class("wheel-center");
        center_button.set_size_request(WHEEL_CENTER, WHEEL_CENTER);
        center_button.set_tooltip_text(Some("閉じる"));
        center_button.set_focus_on_click(false);
        layout.put(
            &center_button,
            center - WHEEL_CENTER as f64 / 2.0,
            center - WHEEL_CENTER as f64 / 2.0,
        );

        root.put(&circle, 0.0, 0.0);

        Self {
            root,
            circle,
            buttons,
            center: center_button,
            position: Rc::new(Cell::new((0.0, 0.0))),
        }
    }

    fn open(&self, x: f64, y: f64, width: f64, height: f64) {
        let size = WHEEL_SIZE as f64;
        let max_x = (width - size).max(0.0);
        let max_y = (height - size).max(0.0);
        let px = (x - size / 2.0).clamp(0.0, max_x);
        let py = (y - size / 2.0).clamp(0.0, max_y);
        self.position.set((px, py));
        self.root.move_(&self.circle, px, py);
        self.root.set_visible(true);
    }

    fn close(&self) {
        self.root.set_visible(false);
    }

    fn is_open(&self) -> bool {
        self.root.is_visible()
    }
}

struct RowWidgets {
    title: gtk::Label,
    subtitle: gtk::Label,
}

struct AppState {
    metas: Vec<NoteMeta>,
    displayed: Vec<String>,
    rows: HashMap<String, RowWidgets>,
    active_id: Option<String>,
    active_kind: Option<NoteKind>,
    active_tool: Tool,
    pen_color: String,
    highlight_color: String,
    pen_size: f64,
    highlighter_size: f64,
    eraser_size: f64,
    pending: HashMap<String, Note>,
    save_source: Option<glib::SourceId>,
    rebuilding: bool,
    adjusting: bool,
    syncing_toggle: bool,
}

impl AppState {
    fn new() -> Self {
        Self {
            metas: Vec::new(),
            displayed: Vec::new(),
            rows: HashMap::new(),
            active_id: None,
            active_kind: None,
            active_tool: Tool::Pen,
            pen_color: PEN_COLORS[0].1.to_string(),
            highlight_color: HIGHLIGHT_COLORS[0].1.to_string(),
            pen_size: 3.2,
            highlighter_size: 20.0,
            eraser_size: 26.0,
            pending: HashMap::new(),
            save_source: None,
            rebuilding: false,
            adjusting: false,
            syncing_toggle: false,
        }
    }
}

thread_local! {
    /// Keeps the Rc-based application state alive for the whole GTK main loop.
    static APP_HOLDER: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
}

struct App {
    window: adw::ApplicationWindow,
    toast: adw::ToastOverlay,
    split: adw::OverlaySplitView,
    stack: gtk::Stack,
    canvas: Canvas,
    note_list: gtk::ListBox,
    search: gtk::SearchEntry,
    window_title: adw::WindowTitle,
    sidebar_toggle: gtk::ToggleButton,
    undo_btn: gtk::Button,
    redo_btn: gtk::Button,
    wheel: ToolWheel,
    content_menu: gtk::MenuButton,
    add_menu: gtk::MenuButton,
    toolbar: Toolbar,
    page_pill: gtk::Box,
    zoom_pill: gtk::Box,
    page_label: gtk::Label,
    zoom_label: gtk::Button,
    prev_btn: gtk::Button,
    next_btn: gtk::Button,
    add_page_btn: gtk::Button,
    store: RefCell<Store>,
    state: RefCell<AppState>,
}

pub fn build(application: &adw::Application) {
    let store = match Store::open() {
        Ok(store) => store,
        Err(error) => {
            show_error(application, &error);
            return;
        }
    };

    let window = adw::ApplicationWindow::builder()
        .application(application)
        .title("Sumi")
        .default_width(1260)
        .default_height(860)
        .width_request(640)
        .height_request(460)
        .build();
    window.set_icon_name(Some("sumi-app"));

    let canvas = Canvas::new();
    canvas.area.set_hexpand(true);
    canvas.area.set_vexpand(true);

    let toolbar = build_toolbar();
    toolbar.root.set_halign(gtk::Align::Center);
    toolbar.root.set_valign(gtk::Align::End);
    toolbar.root.set_margin_bottom(22);

    let (page_pill, page_label, prev_btn, next_btn, add_page_btn) = build_page_pill();
    page_pill.set_halign(gtk::Align::Start);
    page_pill.set_valign(gtk::Align::Start);
    page_pill.set_margin_start(18);
    page_pill.set_margin_top(18);

    let (zoom_pill, zoom_label) = build_zoom_pill();
    zoom_pill.set_halign(gtk::Align::End);
    zoom_pill.set_valign(gtk::Align::End);
    zoom_pill.set_margin_end(18);
    zoom_pill.set_margin_bottom(18);

    let wheel = ToolWheel::new();

    let canvas_overlay = gtk::Overlay::new();
    canvas_overlay.set_child(Some(&canvas.area));
    canvas_overlay.add_overlay(&toolbar.root);
    canvas_overlay.add_overlay(&page_pill);
    canvas_overlay.add_overlay(&zoom_pill);
    canvas_overlay.add_overlay(&wheel.root);
    canvas_overlay.set_hexpand(true);
    canvas_overlay.set_vexpand(true);

    toolbar.root.set_visible(false);
    page_pill.set_visible(false);
    zoom_pill.set_visible(false);

    let stack = gtk::Stack::new();
    stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    stack.set_transition_duration(160);
    stack.set_hexpand(true);
    stack.set_vexpand(true);
    stack.add_named(&canvas_overlay, Some("canvas"));

    let window_title = adw::WindowTitle::new("Sumi", "ノートを選択してください");

    let content_toolbar = adw::ToolbarView::new();
    let content_header = adw::HeaderBar::new();
    content_header.set_show_start_title_buttons(false);

    let sidebar_toggle = gtk::ToggleButton::new();
    sidebar_toggle.set_icon_name("sidebar-show-symbolic");
    sidebar_toggle.set_tooltip_text(Some("サイドバー"));
    sidebar_toggle.add_css_class("flat");
    sidebar_toggle.set_active(true);
    content_header.pack_start(&sidebar_toggle);

    let undo_btn = gtk::Button::from_icon_name("edit-undo-symbolic");
    undo_btn.set_tooltip_text(Some("元に戻す (Ctrl+Z)"));
    undo_btn.add_css_class("flat");
    undo_btn.set_sensitive(false);
    content_header.pack_start(&undo_btn);

    let redo_btn = gtk::Button::from_icon_name("edit-redo-symbolic");
    redo_btn.set_tooltip_text(Some("やり直す (Ctrl+Shift+Z)"));
    redo_btn.add_css_class("flat");
    redo_btn.set_sensitive(false);
    content_header.pack_start(&redo_btn);

    content_header.set_title_widget(Some(&window_title));

    let content_menu = gtk::MenuButton::new();
    content_menu.set_icon_name("open-menu-symbolic");
    content_menu.set_tooltip_text(Some("ノートメニュー"));
    content_menu.add_css_class("flat");
    content_menu.set_sensitive(false);
    content_header.pack_end(&content_menu);

    content_toolbar.add_top_bar(&content_header);
    content_toolbar.set_content(Some(&stack));
    content_toolbar.set_vexpand(true);

    let note_list = gtk::ListBox::new();
    note_list.set_selection_mode(gtk::SelectionMode::Single);
    note_list.add_css_class("navigation-sidebar");
    note_list.set_activate_on_single_click(true);

    let sidebar_toolbar = adw::ToolbarView::new();
    sidebar_toolbar.add_css_class("sumi-sidebar");
    let sidebar_header = adw::HeaderBar::new();
    sidebar_header.set_show_start_title_buttons(false);
    sidebar_header.set_show_end_title_buttons(false);
    sidebar_header.set_title_widget(Some(&adw::WindowTitle::new("Sumi", "手書きノート")));

    let add_menu = gtk::MenuButton::new();
    add_menu.set_icon_name("list-add-symbolic");
    add_menu.set_tooltip_text(Some("新規ノート"));
    add_menu.add_css_class("flat");
    sidebar_header.pack_end(&add_menu);
    sidebar_toolbar.add_top_bar(&sidebar_header);

    let sidebar_body = gtk::Box::new(Orientation::Vertical, 0);
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("ノートを検索"));
    search.set_margin_top(2);
    search.set_margin_bottom(8);
    search.set_margin_start(12);
    search.set_margin_end(12);
    search.add_css_class("sumi-search");

    let scroller = gtk::ScrolledWindow::new();
    scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroller.set_vexpand(true);
    scroller.set_child(Some(&note_list));

    let placeholder = gtk::Label::new(Some("ノートがありません"));
    placeholder.add_css_class("dim-label");
    placeholder.set_margin_top(28);
    note_list.set_placeholder(Some(&placeholder));

    sidebar_body.append(&search);
    sidebar_body.append(&scroller);
    sidebar_toolbar.set_content(Some(&sidebar_body));

    let split = adw::OverlaySplitView::new();
    split.set_sidebar(Some(&sidebar_toolbar));
    split.set_content(Some(&content_toolbar));
    split.set_show_sidebar(true);
    split.set_sidebar_width_unit(adw::LengthUnit::Px);
    split.set_sidebar_width_fraction(312.0);
    split.set_min_sidebar_width(250.0);
    split.set_max_sidebar_width(360.0);
    split.set_enable_show_gesture(true);
    split.set_enable_hide_gesture(true);

    let toast = adw::ToastOverlay::new();
    toast.set_child(Some(&split));
    window.set_content(Some(&toast));

    let breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MaxWidth,
        820.0,
        adw::LengthUnit::Px,
    ));
    breakpoint.add_setter(&split, "collapsed", Some(&true.to_value()));
    breakpoint.add_setter(&split, "show-sidebar", Some(&false.to_value()));
    window.add_breakpoint(breakpoint);

    let app = Rc::new(App {
        window,
        toast,
        split,
        stack,
        canvas,
        note_list,
        search,
        window_title,
        sidebar_toggle,
        undo_btn,
        redo_btn,
        wheel,
        content_menu,
        add_menu,
        toolbar,
        page_pill,
        zoom_pill,
        page_label,
        zoom_label,
        prev_btn,
        next_btn,
        add_page_btn,
        store: RefCell::new(store),
        state: RefCell::new(AppState::new()),
    });

    app.wire();
    app.window.present();
    APP_HOLDER.with(|slot| {
        *slot.borrow_mut() = Some(app);
    });
}

fn show_error(application: &adw::Application, message: &str) {
    let window = adw::ApplicationWindow::builder()
        .application(application)
        .title("Sumi")
        .default_width(560)
        .default_height(360)
        .build();
    let page = adw::StatusPage::new();
    page.set_icon_name(Some("dialog-error-symbolic"));
    page.set_title("Sumiを起動できません");
    page.set_description(Some(message));
    window.set_content(Some(&page));
    window.present();
}

fn build_toolbar() -> Toolbar {
    let root = gtk::Box::new(Orientation::Horizontal, 7);
    root.add_css_class("sumi-toolbar");

    let pen = tool_button("sumi-pen-symbolic", "ペン (1)");
    let highlighter = tool_button("sumi-highlighter-symbolic", "マーカー (2)");
    let eraser = tool_button("sumi-eraser-symbolic", "消しゴム (3)");
    let hand = tool_button("sumi-hand-symbolic", "移動 (4)");
    highlighter.set_group(Some(&pen));
    eraser.set_group(Some(&pen));
    hand.set_group(Some(&pen));
    pen.set_active(true);

    root.append(&pen);
    root.append(&highlighter);
    root.append(&eraser);
    root.append(&hand);
    root.append(&separator());

    let pen_palette = gtk::Box::new(Orientation::Horizontal, 6);
    let highlight_palette = gtk::Box::new(Orientation::Horizontal, 6);
    let mut swatches = Vec::new();
    for (index, (name, color)) in PEN_COLORS.iter().enumerate() {
        let button = make_swatch(&format!("p{index}"), color);
        pen_palette.append(&button);
        swatches.push((button, color.to_string(), false));
        let _ = name;
    }
    for (index, (name, color)) in HIGHLIGHT_COLORS.iter().enumerate() {
        let button = make_swatch(&format!("h{index}"), color);
        highlight_palette.append(&button);
        swatches.push((button, color.to_string(), true));
        let _ = name;
    }
    highlight_palette.set_visible(false);

    let palette = gtk::Box::new(Orientation::Horizontal, 0);
    palette.append(&pen_palette);
    palette.append(&highlight_palette);
    root.append(&palette);
    root.append(&separator());

    let size = gtk::Scale::with_range(Orientation::Horizontal, 0.8, 14.0, 0.1);
    size.set_draw_value(false);
    size.set_size_request(88, -1);
    size.set_valign(gtk::Align::Center);
    size.add_css_class("sumi-size");
    let size_value = gtk::Label::new(Some("3.2"));
    size_value.add_css_class("sumi-size-value");
    size_value.set_width_chars(3);
    root.append(&size);
    root.append(&size_value);

    Toolbar {
        root,
        pen,
        highlighter,
        eraser,
        hand,
        palette,
        pen_palette,
        highlight_palette,
        size,
        size_value,
        swatches,
    }
}

fn tool_button(icon: &str, tooltip: &str) -> gtk::ToggleButton {
    let button = gtk::ToggleButton::new();
    let image = gtk::Image::from_icon_name(icon);
    image.set_pixel_size(19);
    button.set_child(Some(&image));
    button.set_tooltip_text(Some(tooltip));
    button.add_css_class("tool");
    button.set_focus_on_click(false);
    button
}

fn make_swatch(id: &str, color: &str) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("swatch");
    button.set_tooltip_text(Some(color));
    button.set_focus_on_click(false);
    button.set_size_request(21, 21);
    button.set_valign(gtk::Align::Center);

    let class = format!("swatch-{id}");
    button.add_css_class(&class);
    if let Some(display) = gdk::Display::default() {
        let provider = gtk::CssProvider::new();
        provider.load_from_data(&format!(
            "button.{class} {{ background-color: {color}; }}"
        ));
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
    button
}

fn separator() -> gtk::Separator {
    let sep = gtk::Separator::new(Orientation::Vertical);
    sep.set_margin_top(4);
    sep.set_margin_bottom(4);
    sep.set_margin_start(2);
    sep.set_margin_end(2);
    sep
}

fn build_page_pill() -> (gtk::Box, gtk::Label, gtk::Button, gtk::Button, gtk::Button) {
    let pill = gtk::Box::new(Orientation::Horizontal, 2);
    pill.add_css_class("sumi-pill");

    let prev = gtk::Button::from_icon_name("go-up-symbolic");
    prev.set_tooltip_text(Some("前のページ"));
    prev.add_css_class("flat");
    let next = gtk::Button::from_icon_name("go-down-symbolic");
    next.set_tooltip_text(Some("次のページ"));
    next.add_css_class("flat");

    let label = gtk::Label::new(Some("1 / 1"));
    label.set_width_chars(6);
    label.add_css_class("sumi-pill-label");

    let add = gtk::Button::from_icon_name("list-add-symbolic");
    add.set_tooltip_text(Some("ページを追加"));
    add.add_css_class("flat");

    pill.append(&prev);
    pill.append(&label);
    pill.append(&next);
    pill.append(&separator());
    pill.append(&add);
    (pill, label, prev, next, add)
}

fn build_zoom_pill() -> (gtk::Box, gtk::Button) {
    let pill = gtk::Box::new(Orientation::Horizontal, 2);
    pill.add_css_class("sumi-pill");

    let minus = gtk::Button::from_icon_name("zoom-out-symbolic");
    minus.set_tooltip_text(Some("縮小"));
    minus.add_css_class("flat");
    let label = gtk::Button::with_label("100%");
    label.set_tooltip_text(Some("100%に戻す"));
    label.add_css_class("flat");
    label.add_css_class("sumi-zoom-label");
    let plus = gtk::Button::from_icon_name("zoom-in-symbolic");
    plus.set_tooltip_text(Some("拡大"));
    plus.add_css_class("flat");
    let fit = gtk::Button::from_icon_name("zoom-fit-best-symbolic");
    fit.set_tooltip_text(Some("全体を表示"));
    fit.add_css_class("flat");

    pill.append(&minus);
    pill.append(&label);
    pill.append(&plus);
    pill.append(&separator());
    pill.append(&fit);
    (pill, label)
}

impl App {
    fn wire(self: &Rc<Self>) {
        // Canvas -> app.
        {
            let weak = Rc::downgrade(self);
            let weak_view = Rc::downgrade(self);
            self.canvas.set_callbacks(
                move || {
                    if let Some(app) = weak.upgrade() {
                        if let Some(note) = app.canvas.note() {
                            app.update_row(&note);
                            app.update_header(&note);
                            app.schedule_save(note);
                        }
                    }
                },
                move |info| {
                    if let Some(app) = weak_view.upgrade() {
                        app.update_view(info);
                    }
                },
            );
        }

        // Tools.
        for (tool, button) in [
            (Tool::Pen, self.toolbar.pen.clone()),
            (Tool::Highlighter, self.toolbar.highlighter.clone()),
            (Tool::Eraser, self.toolbar.eraser.clone()),
            (Tool::Hand, self.toolbar.hand.clone()),
        ] {
            let weak = Rc::downgrade(self);
            button.connect_toggled(move |button| {
                if button.is_active() {
                    if let Some(app) = weak.upgrade() {
                        app.apply_tool(tool);
                    }
                }
            });
        }

        // Palette.
        for (button, color, highlighter) in &self.toolbar.swatches {
            let weak = Rc::downgrade(self);
            let color = color.clone();
            let highlighter = *highlighter;
            button.connect_clicked(move |_| {
                if let Some(app) = weak.upgrade() {
                    app.choose_color(&color, highlighter);
                }
            });
        }

        {
            let weak = Rc::downgrade(self);
            self.toolbar.size.connect_value_changed(move |scale| {
                if let Some(app) = weak.upgrade() {
                    if app.state.borrow().adjusting {
                        return;
                    }
                    let value = scale.value();
                    let tool = app.state.borrow().active_tool;
                    {
                        let mut st = app.state.borrow_mut();
                        match tool {
                            Tool::Pen => st.pen_size = value,
                            Tool::Highlighter => st.highlighter_size = value,
                            Tool::Eraser => st.eraser_size = value,
                            Tool::Hand => {}
                        }
                    }
                    match tool {
                        Tool::Pen => app.canvas.set_pen_size(value),
                        Tool::Highlighter => app.canvas.set_highlighter_size(value),
                        Tool::Eraser => app.canvas.set_eraser_size(value),
                        Tool::Hand => {}
                    }
                    app.toolbar.size_value.set_text(&format!("{value:.1}"));
                }
            });
        }

        // Undo / redo.
        {
            let weak = Rc::downgrade(self);
            self.undo_btn.connect_clicked(move |_| {
                if let Some(app) = weak.upgrade() {
                    app.canvas.undo();
                    app.canvas.area.grab_focus();
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.redo_btn.connect_clicked(move |_| {
                if let Some(app) = weak.upgrade() {
                    app.canvas.redo();
                    app.canvas.area.grab_focus();
                }
            });
        }

        // Page controls.
        {
            let weak = Rc::downgrade(self);
            self.prev_btn.connect_clicked(move |_| {
                if let Some(app) = weak.upgrade() {
                    app.canvas.step_page(-1);
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.next_btn.connect_clicked(move |_| {
                if let Some(app) = weak.upgrade() {
                    app.canvas.step_page(1);
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.add_page_btn.connect_clicked(move |_| {
                if let Some(app) = weak.upgrade() {
                    app.canvas.add_page();
                    app.canvas.area.grab_focus();
                }
            });
        }

        // Zoom controls.
        {
            let weak = Rc::downgrade(self);
            self.zoom_label.connect_clicked(move |_| {
                if let Some(app) = weak.upgrade() {
                    app.canvas.zoom_reset();
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.zoom_pill.first_child().and_then(|w| w.next_sibling()).map(|_| ());
            // The three zoom buttons are wired by walking the pill's children.
            let mut child = self.zoom_pill.first_child();
            let mut index = 0;
            while let Some(widget) = child {
                if let Some(button) = widget.downcast_ref::<gtk::Button>() {
                    let weak = weak.clone();
                    match index {
                        0 => {
                            button.connect_clicked(move |_| {
                                if let Some(app) = weak.upgrade() {
                                    app.canvas.zoom_out();
                                }
                            });
                        }
                        2 => {
                            button.connect_clicked(move |_| {
                                if let Some(app) = weak.upgrade() {
                                    app.canvas.zoom_in();
                                }
                            });
                        }
                        4 => {
                            button.connect_clicked(move |_| {
                                if let Some(app) = weak.upgrade() {
                                    app.canvas.fit();
                                }
                            });
                        }
                        _ => {}
                    }
                }
                child = widget.next_sibling();
                index += 1;
            }
        }

        // Search filter.
        {
            let weak = Rc::downgrade(self);
            self.search.connect_search_changed(move |_| {
                if let Some(app) = weak.upgrade() {
                    app.refresh_list();
                }
            });
        }

        // Sidebar selection.
        {
            let weak = Rc::downgrade(self);
            self.note_list.connect_row_selected(move |_, row| {
                let Some(row) = row else { return };
                let Some(app) = weak.upgrade() else { return };
                if app.state.borrow().rebuilding {
                    return;
                }
                let index = row.index();
                if index < 0 {
                    return;
                }
                let id = app
                    .state
                    .borrow()
                    .displayed
                    .get(index as usize)
                    .cloned();
                if let Some(id) = id {
                    if app.state.borrow().active_id.as_deref() == Some(id.as_str()) {
                        return;
                    }
                    app.select_note(&id);
                }
            });
        }

        // Sidebar toggle.
        {
            let weak = Rc::downgrade(self);
            self.sidebar_toggle.connect_toggled(move |button| {
                if let Some(app) = weak.upgrade() {
                    if app.state.borrow().syncing_toggle {
                        return;
                    }
                    app.split.set_show_sidebar(button.is_active());
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            let toggle = self.sidebar_toggle.clone();
            self.split.connect_show_sidebar_notify(move |split| {
                let Some(app) = weak.upgrade() else { return };
                {
                    let mut st = app.state.borrow_mut();
                    if st.syncing_toggle {
                        return;
                    }
                    st.syncing_toggle = true;
                }
                toggle.set_active(split.shows_sidebar());
                if let Some(app) = weak.upgrade() {
                    app.state.borrow_mut().syncing_toggle = false;
                }
            });
        }

        // Menus.
        self.build_add_menu();
        self.build_content_menu();
        self.build_empty_page();

        // Window close saves pending changes.
        {
            let weak = Rc::downgrade(self);
            self.window.connect_close_request(move |_| {
                if let Some(app) = weak.upgrade() {
                    app.flush_saves();
                }
                glib::Propagation::Proceed
            });
        }

        self.wire_shortcuts();
        self.wire_tool_wheel();
        self.apply_tool(Tool::Pen);
        self.reload();
    }

    fn wire_tool_wheel(self: &Rc<Self>) {
        for item in &self.wheel.buttons {
            let weak = Rc::downgrade(self);
            let action = item.action;
            item.button.connect_clicked(move |_| {
                if let Some(app) = weak.upgrade() {
                    if crate::canvas::input_debug_enabled() {
                        crate::canvas::input_debug_log("[sumi:wheel] item clicked");
                    }
                    app.run_wheel_action(action);
                    app.wheel.close();
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.wheel.center.connect_clicked(move |_| {
                if let Some(app) = weak.upgrade() {
                    app.wheel.close();
                    app.canvas.area.grab_focus();
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            let position = self.wheel.position.clone();
            let click = gtk::GestureClick::new();
            // Close only on release. Closing on press can cancel the release
            // that GtkButton needs in order to emit `clicked`, making wheel
            // items appear unresponsive.
            click.connect_released(move |_, _, x, y| {
                let (px, py) = position.get();
                let inside = x >= px
                    && x <= px + WHEEL_SIZE as f64
                    && y >= py
                    && y <= py + WHEEL_SIZE as f64;
                if !inside {
                    if let Some(app) = weak.upgrade() {
                        app.wheel.close();
                        app.canvas.area.grab_focus();
                    }
                }
            });
            self.wheel.root.add_controller(click);
        }
    }

    fn open_tool_wheel(self: &Rc<Self>) {
        let (x, y) = self.canvas.pointer_position();
        let width = self.canvas.area.width() as f64;
        let height = self.canvas.area.height() as f64;
        self.wheel.open(x, y, width, height);
        self.canvas.area.grab_focus();
        if crate::canvas::input_debug_enabled() {
            crate::canvas::input_debug_log(&format!(
                "[sumi:wheel] open x={x:.1} y={y:.1} area={width:.1}x{height:.1} visible={}",
                self.wheel.is_open()
            ));
        }
    }

    fn run_wheel_action(self: &Rc<Self>, action: WheelAction) {
        match action {
            WheelAction::Tool(tool) => self.activate_tool(tool),
            WheelAction::Undo => {
                if !self.canvas.undo() {
                    self.toast("元に戻す操作はありません");
                }
            }
            WheelAction::Redo => {
                if !self.canvas.redo() {
                    self.toast("やり直す操作はありません");
                }
            }
        }
        self.canvas.area.grab_focus();
    }

    fn wire_shortcuts(self: &Rc<Self>) {
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        // Lenovo Tab Pen Plus reports its single button as Ctrl+1..Ctrl+6
        // through a virtual keyboard. Use those as an eraser toggle.
        let pen_button_key: Rc<Cell<Option<gdk::Key>>> = Rc::new(Cell::new(None));
        let pen_toggle_previous: Rc<Cell<Option<Tool>>> = Rc::new(Cell::new(None));
        {
            let weak = Rc::downgrade(self);
            let pen_button_key = pen_button_key.clone();
            let pen_toggle_previous = pen_toggle_previous.clone();
            keys.connect_key_pressed(move |controller, key, _, mods| {
                let Some(app) = weak.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                if text_focus(&app.window) {
                    return glib::Propagation::Proceed;
                }
                if crate::canvas::input_debug_enabled() {
                    let device = controller
                        .current_event_device()
                        .map(|device| device.name().to_string());
                    crate::canvas::input_debug_log(&format!(
                        "[sumi:input] key_pressed key={key:?} mods={mods:?} device={device:?}"
                    ));
                }
                let from_stylus = controller
                    .current_event_device()
                    .map(|device| crate::canvas::device_looks_like_stylus(&device))
                    .unwrap_or(false);
                if from_stylus {
                    // Bluetooth pens may send their buttons as keyboard keys.
                    app.canvas.stylus_key_down(key.into_glib());
                    return glib::Propagation::Stop;
                }
                if key == gdk::Key::Escape && app.wheel.is_open() {
                    app.wheel.close();
                    app.canvas.area.grab_focus();
                    return glib::Propagation::Stop;
                }

                let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
                let shift = mods.contains(gdk::ModifierType::SHIFT_MASK);

                // The tbpen-buttons bridge (and some pen firmwares) sends
                // Ctrl+1/4/5/6. Keep these semantic instead of toggling here:
                // 1 = brush, 4 = eraser, 5 = double press (wheel),
                // 6 = long press (wheel).
                if ctrl {
                    match key {
                        gdk::Key::_1 => {
                            pen_button_key.set(Some(key));
                            let restore = pen_toggle_previous.take().unwrap_or(Tool::Pen);
                            app.activate_tool(restore);
                            return glib::Propagation::Stop;
                        }
                        gdk::Key::_4 => {
                            pen_button_key.set(Some(key));
                            let active = app.state.borrow().active_tool;
                            if active != Tool::Eraser {
                                pen_toggle_previous.set(Some(active));
                            }
                            app.activate_tool(Tool::Eraser);
                            return glib::Propagation::Stop;
                        }
                        gdk::Key::_5 | gdk::Key::_6 => {
                            // 0x0C0601 (double press) is forwarded as Ctrl+5,
                            // and long press as Ctrl+6. Both open the wheel.
                            pen_button_key.set(Some(key));
                            app.open_tool_wheel();
                            return glib::Propagation::Stop;
                        }
                        _ => {}
                    }
                }

                if key == gdk::Key::space && !ctrl {
                    app.canvas.set_space_down(true);
                    return glib::Propagation::Stop;
                }
                if ctrl && (key == gdk::Key::z || key == gdk::Key::Z) {
                    if shift {
                        app.canvas.redo();
                    } else {
                        app.canvas.undo();
                    }
                    return glib::Propagation::Stop;
                }
                if ctrl && (key == gdk::Key::y || key == gdk::Key::Y) {
                    app.canvas.redo();
                    return glib::Propagation::Stop;
                }
                if ctrl && (key == gdk::Key::equal || key == gdk::Key::plus) {
                    app.canvas.zoom_in();
                    return glib::Propagation::Stop;
                }
                if ctrl && key == gdk::Key::minus {
                    app.canvas.zoom_out();
                    return glib::Propagation::Stop;
                }
                if ctrl && key == gdk::Key::_0 {
                    app.canvas.zoom_reset();
                    return glib::Propagation::Stop;
                }
                if !ctrl && key == gdk::Key::Page_Up {
                    app.canvas.step_page(-1);
                    return glib::Propagation::Stop;
                }
                if !ctrl && key == gdk::Key::Page_Down {
                    app.canvas.step_page(1);
                    return glib::Propagation::Stop;
                }
                if !ctrl {
                    let tool = match key {
                        gdk::Key::_1 => Some(Tool::Pen),
                        gdk::Key::_2 => Some(Tool::Highlighter),
                        gdk::Key::_3 => Some(Tool::Eraser),
                        gdk::Key::_4 => Some(Tool::Hand),
                        _ => None,
                    };
                    if let Some(tool) = tool {
                        app.activate_tool(tool);
                        return glib::Propagation::Stop;
                    }
                    if key == gdk::Key::f || key == gdk::Key::F {
                        app.canvas.fit();
                        return glib::Propagation::Stop;
                    }
                    if key == gdk::Key::w || key == gdk::Key::W {
                        app.open_tool_wheel();
                        return glib::Propagation::Stop;
                    }
                }
                glib::Propagation::Proceed
            });
        }
        {
            let weak = Rc::downgrade(self);
            let pen_button_key = pen_button_key.clone();
            keys.connect_key_released(move |controller, key, _, mods| {
                if crate::canvas::input_debug_enabled() {
                    let device = controller
                        .current_event_device()
                        .map(|device| device.name().to_string());
                    crate::canvas::input_debug_log(&format!(
                        "[sumi:input] key_released key={key:?} mods={mods:?} device={device:?}"
                    ));
                }
                let Some(app) = weak.upgrade() else {
                    return;
                };
                if pen_button_key.get() == Some(key) {
                    pen_button_key.set(None);
                }
                let key_code = key.into_glib();
                let from_stylus = controller
                    .current_event_device()
                    .map(|device| crate::canvas::device_looks_like_stylus(&device))
                    .unwrap_or(false);
                if from_stylus || app.canvas.stylus_key_held(key_code) {
                    app.canvas.stylus_key_up(key_code);
                    return;
                }
                if key == gdk::Key::space {
                    app.canvas.set_space_down(false);
                }
            });
        }
        self.window.add_controller(keys);

        // Some Bluetooth pens report their barrel buttons through a separate
        // mouse device. Catch those anywhere in the window, not only over the
        // canvas, and use them as temporary tool modifiers.
        {
            let weak = Rc::downgrade(self);
            let barrel = gtk::EventControllerLegacy::new();
            barrel.set_propagation_phase(gtk::PropagationPhase::Capture);
            barrel.connect_event(move |_, event| {
                if crate::canvas::input_debug_enabled() {
                    crate::canvas::log_input_event(event);
                }
                let kind = event.event_type();
                if kind != gdk::EventType::ButtonPress
                    && kind != gdk::EventType::ButtonRelease
                {
                    return glib::Propagation::Proceed;
                }
                let Some(app) = weak.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                // The AES pen (source Pen or with a device tool) is handled by
                // the canvas. This path is for separate Bluetooth pen devices.
                let is_bt_pen = event.device_tool().is_none()
                    && event
                        .device()
                        .map(|device| {
                            device.source() != gdk::InputSource::Pen
                                && crate::canvas::device_looks_like_stylus(&device)
                        })
                        .unwrap_or(false);
                if !is_bt_pen {
                    return glib::Propagation::Proceed;
                }
                let Some(button) = event
                    .downcast_ref::<gdk::ButtonEvent>()
                    .map(|button| button.button())
                else {
                    return glib::Propagation::Proceed;
                };
                app.canvas
                    .stylus_mouse_button(button, kind == gdk::EventType::ButtonPress);
                glib::Propagation::Stop
            });
            self.window.add_controller(barrel);
        }
    }

    fn build_add_menu(self: &Rc<Self>) {
        let popover = gtk::Popover::new();
        popover.set_has_arrow(false);
        let body = gtk::Box::new(Orientation::Vertical, 2);
        body.add_css_class("sumi-menu");
        body.set_width_request(218);

        for (kind, title, subtitle) in [
            (NoteKind::Infinite, "無限キャンバス", "ページのない自由なノート"),
            (NoteKind::Paged, "ページノート", "A4ページを重ねるノート"),
            (NoteKind::Pdf, "PDFを読み込む", "PDFに手書きして注釈"),
        ] {
            let button = menu_button(kind.icon(), title, subtitle);
            let weak = Rc::downgrade(self);
            let pop = popover.downgrade();
            button.connect_clicked(move |_| {
                if let Some(pop) = pop.upgrade() {
                    pop.popdown();
                }
                if let Some(app) = weak.upgrade() {
                    app.create_kind(kind);
                }
            });
            body.append(&button);
        }
        popover.set_child(Some(&body));
        self.add_menu.set_popover(Some(&popover));
    }

    fn build_content_menu(self: &Rc<Self>) {
        let popover = gtk::Popover::new();
        popover.set_has_arrow(false);
        let body = gtk::Box::new(Orientation::Vertical, 2);
        body.add_css_class("sumi-menu");
        body.set_width_request(200);

        let rename = menu_button("document-edit-symbolic", "名前を変更", "");
        {
            let weak = Rc::downgrade(self);
            let pop = popover.downgrade();
            rename.connect_clicked(move |_| {
                if let Some(pop) = pop.upgrade() {
                    pop.popdown();
                }
                if let Some(app) = weak.upgrade() {
                    app.rename_current();
                }
            });
        }
        body.append(&rename);

        let fit = menu_button("zoom-fit-best-symbolic", "全体を表示", "");
        {
            let weak = Rc::downgrade(self);
            let pop = popover.downgrade();
            fit.connect_clicked(move |_| {
                if let Some(pop) = pop.upgrade() {
                    pop.popdown();
                }
                if let Some(app) = weak.upgrade() {
                    app.canvas.fit();
                }
            });
        }
        body.append(&fit);

        body.append(&gtk::Separator::new(Orientation::Horizontal));

        let delete = menu_button("edit-delete-symbolic", "削除", "");
        delete.add_css_class("destructive-action");
        {
            let weak = Rc::downgrade(self);
            let pop = popover.downgrade();
            delete.connect_clicked(move |_| {
                if let Some(pop) = pop.upgrade() {
                    pop.popdown();
                }
                if let Some(app) = weak.upgrade() {
                    app.delete_current();
                }
            });
        }
        body.append(&delete);
        popover.set_child(Some(&body));
        self.content_menu.set_popover(Some(&popover));
    }

    fn build_empty_page(self: &Rc<Self>) {
        let page = adw::StatusPage::new();
        page.set_icon_name(Some("sumi-app"));
        page.set_title("何を書きますか？");
        page.set_description(Some(
            "手書きのための、軽いノート。データはこの端末にだけ保存されます。",
        ));

        let cards = gtk::Box::new(Orientation::Horizontal, 14);
        cards.set_homogeneous(true);
        cards.set_halign(gtk::Align::Center);
        cards.set_margin_top(22);
        cards.add_css_class("sumi-empty");

        for (kind, title, desc) in [
            (NoteKind::Infinite, "無限キャンバス", "どこまでも書ける"),
            (NoteKind::Paged, "ページノート", "ページを重ねる"),
            (NoteKind::Pdf, "PDFに注釈", "読み込んで手書き"),
        ] {
            let button = gtk::Button::new();
            button.add_css_class("create-card");
            let body = gtk::Box::new(Orientation::Vertical, 7);
            body.set_margin_top(18);
            body.set_margin_bottom(18);
            body.set_margin_start(18);
            body.set_margin_end(18);
            let icon = gtk::Image::from_icon_name(kind.icon());
            icon.set_pixel_size(30);
            icon.set_margin_bottom(2);
            let label = gtk::Label::new(Some(title));
            label.add_css_class("create-title");
            let desc = gtk::Label::new(Some(desc));
            desc.add_css_class("create-desc");
            body.append(&icon);
            body.append(&label);
            body.append(&desc);
            button.set_child(Some(&body));
            let weak = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                if let Some(app) = weak.upgrade() {
                    app.create_kind(kind);
                }
            });
            cards.append(&button);
        }
        page.set_child(Some(&cards));
        self.stack.add_named(&page, Some("empty"));
        self.stack.set_visible_child_name("empty");
    }

    fn reload(self: &Rc<Self>) {
        let metas = self.store.borrow().list();
        match metas {
            Ok(metas) => {
                self.state.borrow_mut().metas = metas;
                self.refresh_list();
                let first = self.state.borrow().metas.first().map(|m| m.id.clone());
                if let Some(id) = first {
                    self.select_note(&id);
                } else {
                    self.show_empty();
                }
            }
            Err(error) => self.toast(&error),
        }
    }

    fn refresh_list(self: &Rc<Self>) {
        let query = self.search.text().to_string().to_lowercase();
        let metas = self.state.borrow().metas.clone();
        let shown: Vec<NoteMeta> = metas
            .into_iter()
            .filter(|m| query.is_empty() || m.title.to_lowercase().contains(&query))
            .collect();

        {
            let mut st = self.state.borrow_mut();
            st.rebuilding = true;
            st.displayed = shown.iter().map(|m| m.id.clone()).collect();
            st.rows.clear();
        }

        while let Some(child) = self.note_list.first_child() {
            self.note_list.remove(&child);
        }

        for meta in &shown {
            let (row, title, subtitle) = build_note_row(meta);
            self.note_list.append(&row);
            self.state
                .borrow_mut()
                .rows
                .insert(meta.id.clone(), RowWidgets { title, subtitle });
        }

        let active = self.state.borrow().active_id.clone();
        if let Some(active) = active {
            if let Some(index) = self
                .state
                .borrow()
                .displayed
                .iter()
                .position(|id| id == &active)
            {
                if let Some(row) = self.note_list.row_at_index(index as i32) {
                    self.note_list.select_row(Some(&row));
                }
            }
        }
        self.state.borrow_mut().rebuilding = false;
    }

    fn select_note(self: &Rc<Self>, id: &str) {
        if self.state.borrow().active_id.as_deref() == Some(id) && self.canvas.has_note() {
            return;
        }
        self.flush_saves();
        let loaded = self.store.borrow().load(id);
        match loaded {
            Ok(note) => self.open_note(note),
            Err(error) => self.toast(&error),
        }
    }

    fn open_note(self: &Rc<Self>, note: Note) {
        let pdf = if note.kind == NoteKind::Pdf {
            note.pdf_file.as_ref().and_then(|name| {
                let path = self.store.borrow().pdf_path(name);
                PdfDoc::open(&path).ok()
            })
        } else {
            None
        };
        if note.kind == NoteKind::Pdf && pdf.is_none() {
            self.toast("PDFを読み込めません。白紙のページとして開きます。");
        }

        {
            let mut st = self.state.borrow_mut();
            st.active_id = Some(note.id.clone());
            st.active_kind = Some(note.kind);
        }
        self.canvas.set_note(Some(note.clone()), pdf);
        self.update_header(&note);
        self.update_view(self.canvas.view_info());
        self.stack.set_visible_child_name("canvas");
        self.content_menu.set_sensitive(true);
        self.refresh_list();
        if self.split.is_collapsed() {
            self.split.set_show_sidebar(false);
        }
        self.canvas.area.grab_focus();
    }

    fn create_kind(self: &Rc<Self>, kind: NoteKind) {
        if kind == NoteKind::Pdf {
            self.import_pdf();
            return;
        }
        self.flush_saves();
        let title = match kind {
            NoteKind::Infinite => "無題の無限ノート",
            NoteKind::Paged => "無題のページノート",
            NoteKind::Pdf => "",
        };
        let note = Note::new(kind, title);
        if let Err(error) = self.store.borrow().save(&note) {
            self.toast(&error);
            return;
        }
        self.state.borrow_mut().metas.insert(0, note.meta());
        self.state.borrow_mut().active_id = Some(note.id.clone());
        self.open_note(note);
    }

    fn import_pdf(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::builder()
            .title("PDFを読み込む")
            .modal(true)
            .accept_label("読み込む")
            .build();
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("PDF"));
        filter.add_mime_type("application/pdf");
        filter.add_pattern("*.pdf");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        dialog.set_filters(Some(&filters));
        dialog.set_default_filter(Some(&filter));

        let weak = Rc::downgrade(self);
        dialog.open(
            Some(&self.window),
            None::<&gio::Cancellable>,
            move |result| {
                let Some(app) = weak.upgrade() else { return };
                let Ok(file) = result else { return };
                let Some(path) = file.path() else {
                    app.toast("ファイルを読み込めません");
                    return;
                };
                app.window.set_cursor_from_name(Some("wait"));
                let inspected = pdf::inspect(&path);
                app.window.set_cursor_from_name(None);
                let (_count, sizes) = match inspected {
                    Ok(value) => value,
                    Err(error) => {
                        app.toast(&error);
                        return;
                    }
                };
                app.flush_saves();
                let title = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .filter(|s| !s.is_empty())
                    .unwrap_or("PDFノート")
                    .to_string();
                let mut note = Note::from_pdf(title, sizes);
                match app.store.borrow().import_pdf(&note.id, &path) {
                    Ok(name) => note.pdf_file = Some(name),
                    Err(error) => {
                        app.toast(&error);
                        return;
                    }
                }
                if let Err(error) = app.store.borrow().save(&note) {
                    app.toast(&error);
                    return;
                }
                app.state.borrow_mut().metas.insert(0, note.meta());
                app.state.borrow_mut().active_id = Some(note.id.clone());
                app.open_note(note);
            },
        );
    }

    fn show_empty(self: &Rc<Self>) {
        {
            let mut st = self.state.borrow_mut();
            st.active_id = None;
            st.active_kind = None;
        }
        self.canvas.set_note(None, None);
        self.stack.set_visible_child_name("empty");
        self.window_title.set_title("Sumi");
        self.window_title.set_subtitle("ノートを選択してください");
        self.window.set_title(Some("Sumi"));
        self.content_menu.set_sensitive(false);
        self.toolbar.root.set_visible(false);
        self.page_pill.set_visible(false);
        self.zoom_pill.set_visible(false);
        self.undo_btn.set_sensitive(false);
        self.redo_btn.set_sensitive(false);
        self.note_list.unselect_all();
    }

    fn schedule_save(self: &Rc<Self>, note: Note) {
        {
            let mut st = self.state.borrow_mut();
            st.pending.insert(note.id.clone(), note);
            if st.save_source.is_some() {
                return;
            }
            let weak = Rc::downgrade(self);
            st.save_source = Some(glib::timeout_add_local_once(
                Duration::from_millis(520),
                move || {
                    if let Some(app) = weak.upgrade() {
                        app.state.borrow_mut().save_source = None;
                        app.flush_saves();
                    }
                },
            ));
        }
    }

    fn flush_saves(&self) {
        let pending: Vec<Note> = {
            let mut st = self.state.borrow_mut();
            if st.pending.is_empty() {
                return;
            }
            st.pending.drain().map(|(_, note)| note).collect()
        };
        let mut error_message = None;
        for note in pending {
            if let Err(error) = self.store.borrow().save(&note) {
                error_message = Some(error);
                continue;
            }
            self.update_row(&note);
        }
        if let Some(error) = error_message {
            self.toast(&error);
        }
    }

    fn update_view(&self, info: ViewInfo) {
        self.zoom_label.set_label(&format!("{:.0}%", info.zoom * 100.0));
        let has_note = info.kind.is_some();
        self.toolbar.root.set_visible(has_note);
        self.zoom_pill.set_visible(has_note);
        self.undo_btn.set_sensitive(info.can_undo);
        self.redo_btn.set_sensitive(info.can_redo);

        let show_pages = has_note && info.kind != Some(NoteKind::Infinite);
        self.page_pill.set_visible(show_pages);
        let pages = info.page_count.max(1);
        self.page_label
            .set_text(&format!("{} / {}", info.page.min(pages - 1) + 1, pages));
        self.add_page_btn
            .set_visible(info.kind == Some(NoteKind::Paged));
    }

    fn update_header(&self, note: &Note) {
        let title = if note.title.trim().is_empty() {
            "無題のノート"
        } else {
            note.title.as_str()
        };
        let subtitle = match note.kind {
            NoteKind::Infinite => "無限キャンバス".to_string(),
            NoteKind::Paged => format!("ページノート · {}ページ", note.pages.len()),
            NoteKind::Pdf => format!("PDF · {}ページ", note.pages.len()),
        };
        self.window_title.set_title(title);
        self.window_title.set_subtitle(&subtitle);
        self.window.set_title(Some(&format!("{title} — Sumi")));
    }

    fn update_row(&self, note: &Note) {
        let meta = note.meta();
        {
            let mut st = self.state.borrow_mut();
            if let Some(existing) = st.metas.iter_mut().find(|m| m.id == meta.id) {
                *existing = meta.clone();
            }
            if let Some(row) = st.rows.get(&meta.id) {
                let title = if meta.title.trim().is_empty() {
                    "無題のノート".to_string()
                } else {
                    meta.title.clone()
                };
                row.title.set_text(&title);
                row.subtitle.set_text(&subtitle_text(&meta));
            }
        }
    }

    fn apply_tool(&self, tool: Tool) {
        {
            let mut st = self.state.borrow_mut();
            st.active_tool = tool;
        }
        self.canvas.set_tool(tool);
        let color = {
            let st = self.state.borrow();
            match tool {
                Tool::Highlighter => st.highlight_color.clone(),
                _ => st.pen_color.clone(),
            }
        };
        self.canvas.set_color(&color);
        self.update_tool_ui();
    }

    fn activate_tool(self: &Rc<Self>, tool: Tool) {
        let button = self.toolbar.button(tool).clone();
        if button.is_active() {
            self.apply_tool(tool);
        } else {
            button.set_active(true);
        }
    }

    fn choose_color(&self, color: &str, highlighter: bool) {
        let active = self.state.borrow().active_tool;
        {
            let mut st = self.state.borrow_mut();
            if highlighter {
                st.highlight_color = color.to_string();
            } else {
                st.pen_color = color.to_string();
            }
        }
        let applies = (highlighter && active == Tool::Highlighter)
            || (!highlighter && active == Tool::Pen);
        if applies {
            self.canvas.set_color(color);
        }
        self.update_tool_ui();
    }

    fn update_tool_ui(&self) {
        let (tool, pen_color, highlight_color, pen_size, highlighter_size, eraser_size) = {
            let st = self.state.borrow();
            (
                st.active_tool,
                st.pen_color.clone(),
                st.highlight_color.clone(),
                st.pen_size,
                st.highlighter_size,
                st.eraser_size,
            )
        };
        let show_palette = matches!(tool, Tool::Pen | Tool::Highlighter);
        self.toolbar.palette.set_visible(show_palette);
        self.toolbar.pen_palette.set_visible(tool == Tool::Pen);
        self.toolbar
            .highlight_palette
            .set_visible(tool == Tool::Highlighter);
        self.toolbar.size.set_visible(tool != Tool::Hand);
        self.toolbar.size_value.set_visible(tool != Tool::Hand);

        let (lower, upper, value) = match tool {
            Tool::Pen => (0.8, 14.0, pen_size),
            Tool::Highlighter => (8.0, 44.0, highlighter_size),
            Tool::Eraser => (8.0, 60.0, eraser_size),
            Tool::Hand => (0.8, 14.0, pen_size),
        };
        {
            let mut st = self.state.borrow_mut();
            st.adjusting = true;
        }
        let adjustment = self.toolbar.size.adjustment();
        adjustment.set_lower(lower);
        adjustment.set_upper(upper);
        self.toolbar.size.set_value(value);
        self.toolbar.size_value.set_text(&format!("{value:.1}"));
        self.state.borrow_mut().adjusting = false;

        let selected = match tool {
            Tool::Highlighter => &highlight_color,
            _ => &pen_color,
        };
        for (button, color, highlighter) in &self.toolbar.swatches {
            let visible = if *highlighter {
                tool == Tool::Highlighter
            } else {
                tool == Tool::Pen
            };
            let is_selected = visible && color == selected;
            if is_selected {
                button.add_css_class("selected");
            } else {
                button.remove_css_class("selected");
            }
        }
    }

    fn rename_current(self: &Rc<Self>) {
        let Some(note) = self.canvas.note() else { return };
        let dialog = adw::AlertDialog::new(Some("名前を変更"), None);
        let entry = gtk::Entry::new();
        entry.set_text(&note.title);
        entry.set_activates_default(true);
        entry.set_margin_top(4);
        entry.set_margin_bottom(4);
        dialog.set_extra_child(Some(&entry));
        dialog.add_response("cancel", "キャンセル");
        dialog.add_response("ok", "保存");
        dialog.set_default_response(Some("ok"));
        dialog.set_close_response("cancel");
        dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);

        let entry_focus = entry.clone();
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_dialog: &adw::AlertDialog, response: &str| {
            if response != "ok" {
                return;
            }
            let Some(app) = weak.upgrade() else { return };
            let text = entry.text().to_string();
            let title = if text.trim().is_empty() {
                "無題のノート".to_string()
            } else {
                text.trim().to_string()
            };
            app.canvas.set_title(&title);
            if let Some(note) = app.canvas.note() {
                app.update_header(&note);
                app.update_row(&note);
                app.schedule_save(note);
            }
            app.canvas.area.grab_focus();
        });
        dialog.present(Some(&self.window));
        glib::idle_add_local_once(move || {
            entry_focus.grab_focus();
            entry_focus.select_region(0, -1);
        });
    }

    fn delete_current(self: &Rc<Self>) {
        let Some(note) = self.canvas.note() else { return };
        let dialog = adw::AlertDialog::new(
            Some("ノートを削除しますか？"),
            Some("この操作は取り消せません。"),
        );
        dialog.add_response("cancel", "キャンセル");
        dialog.add_response("delete", "削除");
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);

        let weak = Rc::downgrade(self);
        let id = note.id.clone();
        dialog.connect_response(None, move |_dialog: &adw::AlertDialog, response: &str| {
            if response != "delete" {
                return;
            }
            let Some(app) = weak.upgrade() else { return };
            if let Err(error) = app.store.borrow().delete(&id) {
                app.toast(&error);
                return;
            }
            {
                let mut st = app.state.borrow_mut();
                st.pending.remove(&id);
                st.metas.retain(|m| m.id != id);
                st.active_id = None;
                st.active_kind = None;
            }
            app.canvas.set_note(None, None);
            app.refresh_list();
            let next = app.state.borrow().metas.first().map(|m| m.id.clone());
            if let Some(next) = next {
                app.select_note(&next);
            } else {
                app.show_empty();
            }
            app.toast("ノートを削除しました");
        });
        dialog.present(Some(&self.window));
    }

    fn toast(&self, message: &str) {
        let toast = adw::Toast::new(message);
        toast.set_timeout(3);
        self.toast.add_toast(toast);
    }
}

fn build_note_row(meta: &NoteMeta) -> (gtk::ListBoxRow, gtk::Label, gtk::Label) {
    let icon_box = gtk::Box::new(Orientation::Vertical, 0);
    icon_box.set_size_request(36, 36);
    icon_box.set_halign(gtk::Align::Center);
    icon_box.set_valign(gtk::Align::Center);
    icon_box.add_css_class("note-icon");
    let icon = gtk::Image::from_icon_name(meta.kind.icon());
    icon.set_pixel_size(18);
    icon_box.append(&icon);

    let title = gtk::Label::new(Some(if meta.title.trim().is_empty() {
        "無題のノート"
    } else {
        &meta.title
    }));
    title.set_xalign(0.0);
    title.set_ellipsize(pango::EllipsizeMode::End);
    title.set_width_chars(1);
    title.set_hexpand(true);
    title.add_css_class("note-title");

    let subtitle = gtk::Label::new(Some(&subtitle_text(meta)));
    subtitle.set_xalign(0.0);
    subtitle.set_ellipsize(pango::EllipsizeMode::End);
    subtitle.set_width_chars(1);
    subtitle.set_hexpand(true);
    subtitle.add_css_class("note-subtitle");

    let text = gtk::Box::new(Orientation::Vertical, 1);
    text.set_valign(gtk::Align::Center);
    text.set_hexpand(true);
    text.append(&title);
    text.append(&subtitle);

    let body = gtk::Box::new(Orientation::Horizontal, 10);
    body.set_margin_top(6);
    body.set_margin_bottom(6);
    body.set_margin_start(8);
    body.set_margin_end(8);
    body.append(&icon_box);
    body.append(&text);

    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&body));
    row.add_css_class("note-row");
    (row, title, subtitle)
}

fn subtitle_text(meta: &NoteMeta) -> String {
    let scope = match meta.kind {
        NoteKind::Infinite => "無限".to_string(),
        NoteKind::Paged => format!("{}ページ", meta.page_count),
        NoteKind::Pdf => format!("PDF · {}ページ", meta.page_count),
    };
    format!("{scope} · {}", relative_time(meta.updated_at))
}

fn relative_time(timestamp: i64) -> String {
    let diff = now_secs().saturating_sub(timestamp);
    if diff < 60 {
        "たった今".to_string()
    } else if diff < 3600 {
        format!("{}分前", diff / 60)
    } else if diff < 86_400 {
        format!("{}時間前", diff / 3600)
    } else if diff < 172_800 {
        "昨日".to_string()
    } else {
        glib::DateTime::from_unix_local(timestamp)
            .ok()
            .and_then(|dt| dt.format("%Y/%m/%d").ok())
            .map(|s| s.to_string())
            .unwrap_or_default()
    }
}

fn menu_button(icon: &str, label: &str, subtitle: &str) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("menu-item");
    button.set_halign(gtk::Align::Fill);

    let body = gtk::Box::new(Orientation::Horizontal, 10);
    let image = gtk::Image::from_icon_name(icon);
    image.set_pixel_size(17);
    body.append(&image);
    let text = gtk::Box::new(Orientation::Vertical, 0);
    let title = gtk::Label::new(Some(label));
    title.set_xalign(0.0);
    title.add_css_class("menu-item-title");
    text.append(&title);
    if !subtitle.is_empty() {
        let sub = gtk::Label::new(Some(subtitle));
        sub.set_xalign(0.0);
        sub.add_css_class("menu-item-subtitle");
        text.append(&sub);
    }
    body.append(&text);
    button.set_child(Some(&body));
    button
}

fn text_focus(window: &impl IsA<gtk::Window>) -> bool {
    let window: &gtk::Window = window.upcast_ref();
    let mut widget = gtk::prelude::GtkWindowExt::focus(window);
    while let Some(current) = widget {
        if current.is::<gtk::Entry>() || current.is::<gtk::Text>() {
            return true;
        }
        widget = current.parent();
    }
    false
}
