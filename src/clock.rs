use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub enum Clock {
    System,
    Frozen(Arc<Mutex<Instant>>),
}

impl Clock {
    pub fn frozen() -> Clock {
        Clock::Frozen(Arc::new(Mutex::new(Instant::now())))
    }

    pub fn advance(&self, by: Duration) {
        if let Clock::Frozen(now) = self {
            *now.lock().unwrap_or_else(|e| e.into_inner()) += by;
        }
    }

    pub fn now(&self) -> Instant {
        match self {
            Clock::System => Instant::now(),
            Clock::Frozen(now) => *now.lock().unwrap_or_else(|e| e.into_inner()),
        }
    }
}
