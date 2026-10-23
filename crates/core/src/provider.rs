//! The interface every source of search results implements.

use crate::{item::SearchItem, view::View};

/// A source of root-search results. Providers return data only; they never
/// touch the UI and never run actions themselves (see [`crate::item::Command`]).
///
/// Calls are synchronous. `catalog` runs once on a background thread;
/// `answers` and `fallbacks` run on every keystroke, so they must stay well
/// under a millisecond or bound their own work.
pub trait Provider: Send + Sync {
    fn id(&self) -> &'static str;

    /// Items that root search ranks against the query by fuzzy match and
    /// frecency (apps, commands). May block.
    fn catalog(&self) -> Vec<SearchItem> {
        Vec::new()
    }

    /// Results computed from the query itself (a calculator answer, a URL to
    /// open), shown above the ranked items.
    fn answers(&self, _query: &str) -> Vec<SearchItem> {
        Vec::new()
    }

    /// Shown below everything else for a non-empty query (web search).
    fn fallbacks(&self, _query: &str) -> Vec<SearchItem> {
        Vec::new()
    }

    /// The view `id`, for [`Command::OpenProviderView`]: built when opened,
    /// on the UI thread, so keep it to a few milliseconds.
    ///
    /// [`Command::OpenProviderView`]: crate::item::Command::OpenProviderView
    fn view(&self, _id: &str) -> Option<View> {
        None
    }
}
