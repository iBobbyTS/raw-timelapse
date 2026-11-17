mod profile;

include!("app.rs");

fn main() {
    launch(profile::parse);
}
