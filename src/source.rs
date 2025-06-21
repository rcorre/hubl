use std::sync::Arc;

use tokio::sync::mpsc::{Receiver, Sender};

pub trait Source {
    type Item;

    // Start a search, invoking the provided callback with matching items as they are found
    fn start_search_task(&self, query: &str, callback: Arc<(dyn Fn(Self::Item) + Sync + Send)>);

    // Start the preview task.
    // Items can be sent on the sender.
    // Preview content will be returned on the receiver
    fn start_preview_task(&self) -> (Sender<Self::Item>, Receiver<(Self::Item, String)>);
}
