use async_trait::async_trait;

use crate::hooks::traits::{HookHandler, HookResult};

/// Built-in hook for startup prompt boot-script mutation.
///
/// Current implementation is a pass-through placeholder to keep behavior stable.
pub struct BootScriptHook;

#[async_trait]
impl HookHandler for BootScriptHook {
    fn name(&self) -> &str {
        "boot-script"
    }

    fn priority(&self) -> i32 {
        10
    }

    async fn before_prompt_build(&self, prompt: String) -> HookResult<String> {
        HookResult::Continue(prompt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn boot_script_hook_passes_prompt_through() {
        let hook = BootScriptHook;
        match hook.before_prompt_build("prompt".into()).await {
            HookResult::Continue(next) => assert_eq!(next, "prompt"),
            HookResult::Cancel(reason) => panic!("unexpected cancel: {reason}"),
        }
    }
}
