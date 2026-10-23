//! Build-time conversion of vendored TextMate themes into syntect dumps.
//!
//! The vendored `assets/themes/*.tmTheme` plists are parsed here with
//! syntect's `plist-load` feature (host-only build dependency) and written to
//! `$OUT_DIR/<name>.themedump` as compressed syntect dumps. At runtime both
//! native and wasm builds `include_bytes!` and `from_binary` these dumps via
//! `dump-load` (enabled by `parsing`/`default-onig`), so no plist parser or
//! oniguruma is pulled into the wasm build.

use std::path::Path;

use syntect::dumps::dump_binary;
use syntect::highlighting::ThemeSet;

fn main() {
    convert_theme("assets/themes/tokyonight.tmTheme", "tokyonight.themedump");
    // bat's assets: with the `bat` feature, which only native builds can have.
    println!("cargo::rustc-check-cfg=cfg(bat_assets)");
    let native = std::env::var("CARGO_CFG_TARGET_ARCH").is_ok_and(|arch| arch != "wasm32");
    if native && std::env::var_os("CARGO_FEATURE_BAT").is_some() {
        println!("cargo::rustc-cfg=bat_assets");
    }
}

fn convert_theme(src: &str, dump_name: &str) {
    println!("cargo:rerun-if-changed={src}");

    let theme = ThemeSet::get_theme(src)
        .unwrap_or_else(|e| panic!("failed to parse vendored theme {src}: {e}"));
    let dump = dump_binary(&theme);

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR set by cargo");
    let dest = Path::new(&out_dir).join(dump_name);
    std::fs::write(&dest, dump)
        .unwrap_or_else(|e| panic!("failed to write theme dump {}: {e}", dest.display()));
}
