//! Build script: compiles the Slint UI definition (`ui/app.slint`) into
//! Rust code for `slint::include_modules!` in `src/main.rs`.

fn main() {
    if let Err(err) = slint_build::compile("ui/app.slint") {
        panic!("Slint UI compilation failed: {err}");
    }
}
