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

pub const APP_ID: &str = "com.ryouta.Sumi";

fn main() -> glib::ExitCode {
    let mut args: Vec<String> = std::env::args().collect();
    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        println!(
            "sumi {} ({})",
            env!("CARGO_PKG_VERSION"),
            env!("SUMI_GIT_HASH")
        );
        return glib::ExitCode::SUCCESS;
    }

    if args.iter().any(|arg| arg == "--input-debug") {
        std::env::set_var("SUMI_DEBUG_INPUT", "1");
        // GApplication rejects options it does not know, so remove ours
        // before handing the command line over.
        args.retain(|arg| arg != "--input-debug");
    }

    glib::set_application_name("Sumi");
    let application = adw::Application::builder()
        .application_id(APP_ID)
        .build();

    application.connect_startup(|_| {
        icons::install();
        load_css();
        gtk::Window::set_default_icon_name(APP_ID);
    });
    application.connect_activate(|app| {
        app::build(app);
    });

    application.run_with_args(&args)
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
