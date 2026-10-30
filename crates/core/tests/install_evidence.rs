//! Compile and exercise the actual repair journal adapters on each native host
//! while the rest of the installer's processor/transaction port is in progress.
//! This does not stand in for a complete installer or desktop build.
#[allow(dead_code)]
#[path = "../../install/src/repair_evidence.rs"]
mod evidence;
#[allow(dead_code)]
#[path = "../../install/src/repair_staging.rs"]
mod staging;
#[allow(dead_code)]
#[path = "../../install/src/repair_lock.rs"]
mod writer_lock;
