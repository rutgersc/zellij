//! Re-exports the shared `agent-readmodel` types under the `agents` module
//! name the rest of `agent-bar`'s code uses, so call sites read naturally.
//! No agent-bar-specific projection logic — this plugin renders the
//! readmodel as-is, just sorted.

pub use agent_readmodel::{
    Agent, AgentHost, AgentStatus, ReadResult, locate_snapshot, read_readmodel as read,
    read_seen_events,
};
