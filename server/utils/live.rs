use tokio::sync::broadcast;

#[derive(Clone, Debug)]
pub struct Live(broadcast::Sender<u8>);

impl Live {
    pub const ALL: u8 = u8::MAX;
    pub const LIBRARY: u8 = 1;
    pub const SCAN: u8 = 2;

    pub fn new() -> Live {
        Live(broadcast::channel(16).0)
    }

    pub fn notify(&self) {
        let _ = self.0.send(Live::ALL);
    }

    pub fn notify_topics(&self, topics: u8) {
        let _ = self.0.send(topics);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<u8> {
        self.0.subscribe()
    }
}

impl Default for Live {
    fn default() -> Self {
        Live::new()
    }
}
