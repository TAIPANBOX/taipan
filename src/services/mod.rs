//! One module per service this supervisor knows how to build, start, and
//! health-check. Each `start_*` function is self-contained: on any failure
//! (build, spawn, or a healthz timeout) it cleans up whatever it itself
//! started before returning `Err`, so a caller never has to reason about a
//! half-started single service — only about rolling back *earlier*,
//! already-healthy ones (see `commands::up`).

pub mod cloud;
pub mod gateway;
pub mod idryx;
pub mod tokenfuse_build;
pub mod wardryx;

use crate::descriptor::ServiceEntry;
use crate::procutil::Spawned;

/// What a successful `start_*` call hands back to `commands::up`: the
/// tracked process (for the pidfile / later rollback) and the descriptor
/// entry it earned.
pub struct StartedService {
    pub spawned: Spawned,
    pub entry: ServiceEntry,
}
