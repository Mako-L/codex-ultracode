//! Advisory policy for workflow progress; it never changes execution limits.

#[derive(Debug, PartialEq)]
pub(crate) struct LargeWorkflowWarning {
    pub axis: &'static str,
    pub scheduled_agents: usize,
    pub total_tokens: u64,
    pub projected_tokens: u64,
    pub agent_cap: f64,
    pub token_cap: f64,
    pub cap_from_guideline: bool,
}

#[derive(Default)]
pub(crate) struct WorkflowWarningSettings<'a> {
    // None preserves the default identity: medium guidance, 25-agent warning.
    pub size_guideline: Option<&'a str>,
    pub ultracode_active: bool,
    pub agent_threshold: Option<f64>,
    pub token_threshold: Option<f64>,
}

pub(crate) fn large_workflow_warning(
    scheduled_agents: usize,
    started_agents: usize,
    total_tokens: u64,
    settings: &WorkflowWarningSettings<'_>,
) -> Option<LargeWorkflowWarning> {
    if settings.ultracode_active {
        return None;
    }
    let guideline_cap = match settings.size_guideline {
        Some("small") => Some(5.0),
        Some("medium") => Some(15.0),
        Some("large") => Some(50.0),
        _ => None,
    };
    let positive = |value: &f64| value.is_finite() && *value > 0.0;
    let override_cap = settings.agent_threshold.filter(positive);
    let agent_cap = override_cap.or(guideline_cap).unwrap_or(25.0);
    let token_cap = settings
        .token_threshold
        .filter(positive)
        .unwrap_or(1_500_000.0);
    let tokens_per_agent = if started_agents == 0 {
        70_000.0
    } else {
        total_tokens as f64 / started_agents as f64
    };
    let projected_tokens =
        total_tokens.max((tokens_per_agent * scheduled_agents as f64).round() as u64);
    let agents = scheduled_agents as f64 > agent_cap;
    let tokens = projected_tokens as f64 > token_cap;
    if !agents && !tokens {
        return None;
    }
    Some(LargeWorkflowWarning {
        axis: match (agents, tokens) {
            (true, true) => "both",
            (true, false) => "agents",
            _ => "tokens",
        },
        scheduled_agents,
        total_tokens,
        projected_tokens,
        agent_cap,
        token_cap,
        cap_from_guideline: agents && override_cap.is_none() && guideline_cap.is_some(),
    })
}

#[cfg(test)]
#[path = "workflow_advisory_tests.rs"]
mod tests;
