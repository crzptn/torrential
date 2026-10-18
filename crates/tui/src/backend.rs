use torrential_core::protocol::{Change, Event, FilesReply, GetReply};
use torrential_core::{Error, SearchQuery, SearchResult};

/// What the screen asks of the engine. Every method blocks; the TUI calls each one on its own
/// thread, so an implementation can hold one connection per call.
pub trait Backend: Send + Sync + 'static {
    /// Runs a search. `partial` receives the results so far and the sources still pending, each
    /// time a source answers.
    fn search(
        &self,
        query: SearchQuery,
        partial: &mut dyn FnMut(SearchResult, Vec<String>),
    ) -> Result<SearchResult, Error>;
    /// The files of a search result or a download. A download's files carry their progress.
    fn files(&self, reference: &str) -> Result<FilesReply, Error>;
    /// Starts downloading a search result. `files` holds exact paths; `None` means every file.
    fn get(&self, reference: &str, files: Option<Vec<String>>) -> Result<GetReply, Error>;
    fn pause(&self, id: &str) -> Result<Vec<Change>, Error>;
    fn resume(&self, id: &str) -> Result<Vec<Change>, Error>;
    /// Stops tracking a download. Its files stay on disk.
    fn remove(&self, id: &str) -> Result<Vec<Change>, Error>;
    fn verify(&self, id: &str) -> Result<Vec<Change>, Error>;
    /// Streams every download's events, starting with `Event::Snapshot`, until the engine goes away.
    fn watch(&self, event: &mut dyn FnMut(Event)) -> Result<(), Error>;
    /// Free bytes in the default download folder.
    fn free_space(&self) -> Option<u64>;
}
