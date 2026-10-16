use tokio::sync::broadcast;

#[derive(Clone, Debug)]
pub struct Live(broadcast::Sender<()>);

impl Live {
    pub fn new() -> Live {
        Live(broadcast::channel(16).0)
    }

    pub fn notify(&self) {
        let _ = self.0.send(());
    }

    pub fn subscribe(&self) -> broadcast::Receiver<()> {
        self.0.subscribe()
    }
}

impl Default for Live {
    fn default() -> Self {
        Live::new()
    }
}
