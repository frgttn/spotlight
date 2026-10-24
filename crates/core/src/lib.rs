//! Domain types shared by every Spotlight crate. No platform or UI code here.

pub mod clip;
pub mod deeplink;
pub mod frecency;
pub mod item;
pub mod placeholder;
pub mod provider;
pub mod quicklink;
pub mod snippet;
pub mod view;
pub mod web;

/// Stable identity of a search result, `"{provider}:{instance}"`.
/// Frecency and (later) aliases and hotkeys are keyed by it, so it must not
/// change between runs.
pub fn item_id(provider: &str, instance: &str) -> String {
    format!("{provider}:{instance}")
}
