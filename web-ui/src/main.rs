mod app;
mod components;
mod emotes;
mod format;
mod nick_color;
mod protocol;
mod state;
mod ws;

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(app::App);
}
