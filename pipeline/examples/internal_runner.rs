//! Development-only runner for existing low-level graph and settings regressions.

include!("../src/app.rs");

fn main() {
    launch(|args| cli::parse_settings(args, true));
}
