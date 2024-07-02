use std::fmt::Display;

use crate::{AppError, AppResult, Shared};

pub type SharedGasTracker = Shared<GasTracker>;

pub enum GasTracker {
    Limitless { used: u64 },
    Limited { limit: u64, remaining: u64 },
}

impl GasTracker {
    pub fn used(&self) -> u64 {
        match self {
            GasTracker::Limitless { used } => *used,
            GasTracker::Limited { limit, remaining } => limit - remaining,
        }
    }

    pub fn deduct(&mut self, consumed: u64) -> AppResult<()> {
        match self {
            GasTracker::Limitless { used } => {
                *used += consumed;
                Ok(())
            },
            GasTracker::Limited { limit, remaining } => {
                if *remaining < consumed {
                    #[cfg(feature = "tracing")]
                    tracing::warn!(
                        "Out of gas: max: {}, consumed: {}",
                        limit,
                        *limit + consumed - *remaining
                    );

                    Err(AppError::OutOfGas {
                        max: *limit,
                        consumed: *limit + consumed - *remaining,
                    })
                } else {
                    *remaining -= consumed;

                    Ok(())
                }
            },
        }
    }
}

impl Display for GasTracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GasTracker::Limitless { used } => write!(f, "Gas info: limitless, used: {}", used),
            GasTracker::Limited { limit, remaining } => {
                write!(f, "Gas info: limit: {}, used: {}", limit, limit - remaining)
            },
        }
    }
}

impl SharedGasTracker {
    pub fn new_limitless() -> Self {
        Shared::new(GasTracker::Limitless { used: 0 })
    }

    pub fn new_limited(limit: u64) -> Self {
        Shared::new(GasTracker::Limited {
            limit,
            remaining: limit,
        })
    }
}

#[derive(Default)]
pub struct GasResponse {
    pub gas_used: u64,
}
