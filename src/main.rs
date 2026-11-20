mod app;
mod canvas;
mod icons;
mod model;
mod pdf;
mod store;

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use libadwaita as adw;

fn main() -> glib::ExitCode {
    if std::env::args().any(|arg| arg == "--version" || arg == "-V") {
        println!(
            "sumi {} ({})",
            env!("CARGO_PKG_VERSION"),
            env!("SUMI_GIT_HASH")
        );
        return glib::ExitCode::SUCCESS;
    }

    glib::set_application_name("Sumi");
    let application = adw::Application::builder()
        .application_id("com.ryouta.Sumi")
        .build();

    application.connect_startup(|_| {
        icons::install();
        load_css();
        gtk::Window::set_default_icon_name("sumi-app");
    });
    application.connect_activate(|app| {
        app::build(app);
    });

    application.run()
}

fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_data(include_str!("../assets/style.css"));
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}
