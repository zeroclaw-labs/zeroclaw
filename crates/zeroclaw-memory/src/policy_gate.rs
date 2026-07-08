use crate::policy::{PolicyEnforcer, PolicyViolation};
use crate::traits::{Memory, MemoryCategory};

pub async fn validate_store(
    memory: &dyn Memory,
    policy: &PolicyEnforcer,
    namespace: &str,
    category: &MemoryCategory,
) -> Result<(), PolicyViolation> {
    policy.validate_store(namespace, category)?;

    let namespace_count = memory
        .count_in_scope(Some(namespace), None)
        .await
        .unwrap_or(usize::MAX as u64);
    policy.check_namespace_limit(usize::try_from(namespace_count).unwrap_or(usize::MAX))?;

    let category_count = memory
        .count_in_scope(None, Some(category))
        .await
        .unwrap_or(usize::MAX as u64);
    policy.check_category_limit(usize::try_from(category_count).unwrap_or(usize::MAX))?;

    Ok(())
}
