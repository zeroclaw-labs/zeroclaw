//! The memory a pinned session's tools use when they start memory work of
//! their own.
//!
//! A session pinned to its owner's private plane rebinds its memory tools to
//! the routed handle, but two tools reach memory without going through those
//! registry entries: the pipeline runs memory tools it captured when it was
//! built, and the subagent spawner starts a child run that builds its own
//! memory. Both hold a [`SessionMemoryRoute`] shared with the registry they
//! were assembled into, and the registry pins it when the session is pinned.
//! Until then it is empty, and an unowned session never pins it and keeps
//! the agent's shared memory.

use std::sync::{Arc, OnceLock};
use zeroclaw_api::tool::Tool;
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_memory::Memory;

/// The routed memory of a pinned session, with the policy its memory tools
/// run under.
pub struct RoutedMemory {
    pub memory: Arc<dyn Memory>,
    pub security: Arc<SecurityPolicy>,
}

/// One registry's pinned-session memory, set at most once.
#[derive(Default)]
pub struct SessionMemoryRoute {
    routed: OnceLock<RoutedMemory>,
}

impl SessionMemoryRoute {
    /// Pin the route to `memory`. Pinning again with the same handle is a
    /// no-op; pinning with a different handle is refused, so a session's
    /// tools cannot be moved to another plane once they are routed.
    pub fn pin(
        &self,
        memory: Arc<dyn Memory>,
        security: Arc<SecurityPolicy>,
    ) -> anyhow::Result<()> {
        let candidate = Arc::clone(&memory);
        let current = self
            .routed
            .get_or_init(|| RoutedMemory { memory, security });
        if Arc::ptr_eq(&current.memory, &candidate) {
            Ok(())
        } else {
            anyhow::bail!("session memory is already routed to another plane; refusing to re-route")
        }
    }

    /// The routed memory, once the session is pinned.
    pub fn routed(&self) -> Option<&RoutedMemory> {
        self.routed.get()
    }
}

/// The memory tool named `name` over `memory`, or `None` when `name` is not
/// a memory tool.
pub fn memory_tool_over(
    name: &str,
    memory: &Arc<dyn Memory>,
    security: &Arc<SecurityPolicy>,
) -> Option<Arc<dyn Tool>> {
    use crate::{
        memory_export::MemoryExportTool, memory_forget::MemoryForgetTool,
        memory_purge::MemoryPurgeTool, memory_recall::MemoryRecallTool,
        memory_store::MemoryStoreTool,
    };
    let tool: Arc<dyn Tool> = match name {
        "memory_store" => Arc::new(MemoryStoreTool::new(
            Arc::clone(memory),
            Arc::clone(security),
        )),
        "memory_recall" => Arc::new(MemoryRecallTool::new(Arc::clone(memory))),
        "memory_forget" => Arc::new(MemoryForgetTool::new(
            Arc::clone(memory),
            Arc::clone(security),
        )),
        "memory_export" => Arc::new(MemoryExportTool::new(Arc::clone(memory))),
        "memory_purge" => Arc::new(MemoryPurgeTool::new(
            Arc::clone(memory),
            Arc::clone(security),
        )),
        _ => return None,
    };
    Some(tool)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none_memory() -> Arc<dyn Memory> {
        Arc::new(zeroclaw_memory::NoneMemory::new("none"))
    }

    #[test]
    fn a_route_pins_once_and_refuses_another_plane() {
        let route = SessionMemoryRoute::default();
        assert!(route.routed().is_none());
        let owner = none_memory();
        let security = Arc::new(SecurityPolicy::default());
        route
            .pin(Arc::clone(&owner), Arc::clone(&security))
            .unwrap();
        route
            .pin(Arc::clone(&owner), Arc::clone(&security))
            .unwrap();
        assert!(Arc::ptr_eq(&route.routed().unwrap().memory, &owner));
        assert!(route.pin(none_memory(), security).is_err());
        assert!(Arc::ptr_eq(&route.routed().unwrap().memory, &owner));
    }

    #[test]
    fn only_memory_tools_are_rebuilt_over_a_handle() {
        let memory = none_memory();
        let security = Arc::new(SecurityPolicy::default());
        for name in [
            "memory_store",
            "memory_recall",
            "memory_forget",
            "memory_export",
            "memory_purge",
        ] {
            let tool = memory_tool_over(name, &memory, &security).expect(name);
            assert_eq!(tool.name(), name);
        }
        assert!(memory_tool_over("shell", &memory, &security).is_none());
    }
}
