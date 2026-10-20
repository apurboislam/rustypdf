//! rustypdf: render Typst templates into PDF/A-3b documents from JSON.

pub mod api;
pub mod config;
pub mod error;
pub mod fonts;
pub mod images;
pub mod manifest;
pub mod memo;
pub mod metrics;
pub mod registry;
pub mod render;
pub mod world;

/// mimalloc instead of the system allocator: renders allocate heavily, and
/// glibc malloc spends a large share of a render in `malloc`/`free` and in
/// page faults from returning memory to the OS (docs/performance.md).
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;
