// The desktop worker uses the same native process and profile contracts as the
// CLI, but has its own private protocol and never starts an export.
#![allow(dead_code)]
mod desktop;
mod profile;

include!("app.rs");

fn main() {
    proc::install_stop_handlers();
    if let Err(error) = desktop::serve() {
        eprintln!("{}", error.message);
        std::process::exit(error.code);
    }
}
