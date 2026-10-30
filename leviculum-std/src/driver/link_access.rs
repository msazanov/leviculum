//! Direct access to one Link of the running node.
//!
//! The core already keeps per-link state (cipher, attached interface, keepalive timing and
//! counters) and the setters for it on [`Link`]; these two calls hand that to the application
//! instead of mirroring each value through the node facade.
use super::*;
use leviculum_core::link::{Link, LinkId};

impl ReticulumNode {
    /// Run `f` on link `id`, or `None` if the node has no such link.
    ///
    /// `f` runs under the node's core lock, so keep it short and do not call back into the node.
    pub fn with_link<T>(&self, id: &LinkId, f: impl FnOnce(&Link) -> T) -> Option<T> {
        self.inner.lock_recover().link(id).map(f)
    }

    /// As [`Self::with_link`], with the link mutable (for example
    /// [`Link::set_keepalive_override`]).
    pub fn with_link_mut<T>(&self, id: &LinkId, f: impl FnOnce(&mut Link) -> T) -> Option<T> {
        self.inner.lock_recover().link_mut(id).map(f)
    }
}
