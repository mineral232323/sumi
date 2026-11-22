use std::fs;
use std::path::PathBuf;

use gtk::gdk;

const ACTIONS: &[(&str, &str)] = &[
    (
        "sumi-pen-symbolic.svg",
        include_str!("../assets/icons/actions/sumi-pen-symbolic.svg"),
    ),
    (
        "sumi-highlighter-symbolic.svg",
        include_str!("../assets/icons/actions/sumi-highlighter-symbolic.svg"),
    ),
    (
        "sumi-eraser-symbolic.svg",
        include_str!("../assets/icons/actions/sumi-eraser-symbolic.svg"),
    ),
    (
        "sumi-select-symbolic.svg",
        include_str!("../assets/icons/actions/sumi-select-symbolic.svg"),
    ),
    (
        "sumi-hand-symbolic.svg",
        include_str!("../assets/icons/actions/sumi-hand-symbolic.svg"),
    ),
    (
        "sumi-shape-symbolic.svg",
        include_str!("../assets/icons/actions/sumi-shape-symbolic.svg"),
    ),
    (
        "sumi-line-symbolic.svg",
        include_str!("../assets/icons/actions/sumi-line-symbolic.svg"),
    ),
    (
        "sumi-rect-symbolic.svg",
        include_str!("../assets/icons/actions/sumi-rect-symbolic.svg"),
    ),
    (
        "sumi-ellipse-symbolic.svg",
        include_str!("../assets/icons/actions/sumi-ellipse-symbolic.svg"),
    ),
    (
        "sumi-triangle-symbolic.svg",
        include_str!("../assets/icons/actions/sumi-triangle-symbolic.svg"),
    ),
    (
        "sumi-infinite-symbolic.svg",
        include_str!("../assets/icons/actions/sumi-infinite-symbolic.svg"),
    ),
    (
        "sumi-page-symbolic.svg",
        include_str!("../assets/icons/actions/sumi-page-symbolic.svg"),
    ),
    (
        "sumi-pdf-symbolic.svg",
        include_str!("../assets/icons/actions/sumi-pdf-symbolic.svg"),
    ),
];

const APP_ICON: &str = include_str!("../assets/icons/apps/com.ryouta.Sumi.svg");

pub fn install() {
    let Some(display) = gdk::Display::default() else {
        return;
    };
    let root: PathBuf = glib::user_cache_dir().join("sumi").join("icons");
    let actions = root.join("hicolor").join("scalable").join("actions");
    let apps = root.join("hicolor").join("scalable").join("apps");
    let _ = fs::create_dir_all(&actions);
    let _ = fs::create_dir_all(&apps);
    for (name, data) in ACTIONS {
        let _ = fs::write(actions.join(name), data);
    }
    let _ = fs::write(apps.join(format!("{}.svg", crate::APP_ID)), APP_ICON);

    let theme = gtk::IconTheme::for_display(&display);
    theme.add_search_path(&root);
    gtk::Window::set_default_icon_name(crate::APP_ID);
}
