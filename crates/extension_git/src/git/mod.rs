//! Git reading and command construction.
//!
//! Every command goes through Warp's `execution.run` rather than being spawned
//! here, so the same code path serves a local repository and one on the far
//! side of an SSH session. Nothing in this module executes anything.
pub mod branches;
pub mod commits;
pub mod operations;
pub mod status;

pub use branches::{Branch, BranchScope};
pub use commits::Commit;
pub use status::{ChangeKind, FileChange, Status};
