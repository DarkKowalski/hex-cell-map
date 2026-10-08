use anyhow::{Result, bail};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::Sender,
};

#[derive(Debug, Clone)]
pub struct Progress {
    pub fraction: f32,
    pub message: String,
}

#[derive(Clone, Default)]
pub struct JobContext {
    cancelled: Arc<AtomicBool>,
    progress: Option<Sender<Progress>>,
}

impl JobContext {
    pub fn with_progress(sender: Sender<Progress>) -> Self {
        Self {
            progress: Some(sender),
            ..Self::default()
        }
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
    pub fn check(&self) -> Result<()> {
        if self.cancelled.load(Ordering::Relaxed) {
            bail!("Generation cancelled");
        }
        Ok(())
    }
    pub fn report(&self, fraction: f32, message: impl Into<String>) -> Result<()> {
        self.check()?;
        let progress = Progress {
            fraction: fraction.clamp(0., 1.),
            message: message.into(),
        };
        if let Some(sender) = &self.progress {
            let _ = sender.send(progress);
        }
        Ok(())
    }
}
