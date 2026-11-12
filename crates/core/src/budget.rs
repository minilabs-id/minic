//! Batas anggaran satu sesi.

#[derive(Debug, Clone)]
pub struct BudgetPolicy {
    pub max_steps: u32,
    pub context_window_tokens: u32,
    pub tool_result_max_tokens: u32,
}

impl Default for BudgetPolicy {
    fn default() -> Self {
        Self {
            max_steps: 50,
            context_window_tokens: 128_000,
            tool_result_max_tokens: 4_096,
        }
    }
}
