//! Command dispatch modules. Each file owns one command; sibling-dependent
//! commands return `CliError::NotWired` with the precise gap.

pub mod adapters;
pub mod check_victoriametrics;
pub mod decode_fixture;
pub mod discover;
pub mod read_once;
pub mod render_metrics;
