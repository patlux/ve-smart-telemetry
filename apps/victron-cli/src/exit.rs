/// Exit codes shared with `victron-collector`.
///
/// Scripts can rely on:
/// - `0` success,
/// - `1` operational failure (probe failed, device unreachable, ...),
/// - `2` usage / configuration error (clap's own parse-error exit),
/// - `3` command not wired yet (requires a sibling crate that is still
///   being built in parallel).
pub const OK: u8 = 0;
pub const RUNTIME: u8 = 1;
/// clap exits 2 on parse errors; kept here for documentation symmetry.
#[allow(dead_code)]
pub const USAGE: u8 = 2;
pub const NOT_WIRED: u8 = 3;
