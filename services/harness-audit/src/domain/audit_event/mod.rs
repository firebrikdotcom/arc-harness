pub mod aggregate;
pub mod commands;
pub mod events;
// Kept for legacy projector regression coverage; runtime reports use persisted checkpoints.
#[cfg(test)]
pub mod projector;
