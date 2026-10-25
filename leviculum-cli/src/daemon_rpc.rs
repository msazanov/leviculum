//! The client side of the shared-instance RPC lives in
//! `leviculum_std::daemon_rpc`, where lnmsg can reach it too; every binary
//! here keeps its `mod daemon_rpc;` and reads it through this re-export.

pub use leviculum_std::daemon_rpc::*;
