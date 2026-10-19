//! Golden-image tests: page 1 of each template's `sample.json`, rendered to
//! PNG and compared with `tests/golden/<template>.png`.
//!
//! Approve intended changes with `just golden-update` (sets UPDATE_GOLDEN=1),
//! and say why in the PR.

mod common;

use std::path::PathBuf;

const PIXEL_PER_PT: f32 = 2.0;
/// A channel differing by more than this counts as a changed pixel.
const CHANNEL_TOLERANCE: u8 = 16;
/// Fraction of pixels allowed to change (anti-aliasing noise).
const MAX_CHANGED_FRACTION: f64 = 0.001;

fn check_golden(template: &str) {
    let doc = common::compile(template, &common::sample(template)).expect("sample compiles");
    let png = rustypdf::render::page_png(&doc, 0, PIXEL_PER_PT).expect("page 1 renders");
    let path: PathBuf = common::repo()
        .join("tests/golden")
        .join(format!("{template}.png"));

    if std::env::var_os("UPDATE_GOLDEN").is_some() || !path.exists() {
        if std::env::var_os("CI").is_some() && !path.exists() {
            panic!(
                "missing golden image {}; run `just golden-update`",
                path.display()
            );
        }
        std::fs::write(&path, &png).unwrap();
        eprintln!("updated {}", path.display());
        return;
    }

    let expected = tiny_skia::Pixmap::decode_png(&std::fs::read(&path).unwrap()).unwrap();
    let actual = tiny_skia::Pixmap::decode_png(&png).unwrap();
    let actual_path = path.with_extension("actual.png");
    if (expected.width(), expected.height()) != (actual.width(), actual.height()) {
        std::fs::write(&actual_path, &png).unwrap();
        panic!(
            "{template}: size changed from {}x{} to {}x{} (see {})",
            expected.width(),
            expected.height(),
            actual.width(),
            actual.height(),
            actual_path.display()
        );
    }
    let changed = expected
        .data()
        .chunks_exact(4)
        .zip(actual.data().chunks_exact(4))
        .filter(|(a, b)| {
            a.iter()
                .zip(b.iter())
                .any(|(x, y)| x.abs_diff(*y) > CHANNEL_TOLERANCE)
        })
        .count();
    let fraction = changed as f64 / (actual.width() * actual.height()) as f64;
    if fraction > MAX_CHANGED_FRACTION {
        std::fs::write(&actual_path, &png).unwrap();
        panic!(
            "{template}: {changed} pixels ({:.3}%) differ from {} (see {})",
            fraction * 100.0,
            path.display(),
            actual_path.display()
        );
    }
    let _ = std::fs::remove_file(&actual_path);
}

#[test]
fn golden_bilingual_test() {
    check_golden("bilingual-test");
}

#[test]
fn golden_hello() {
    check_golden("hello");
}
