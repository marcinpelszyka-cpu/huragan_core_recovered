use crate::engine::MigrationTarget;

#[derive(Debug, Clone, Default)]
pub struct HeliusFilterResult {
    pub passed: bool,
    pub reason: String,
    pub creator_score: f64,
    pub top10_holder_pct: f64,
    pub curve_velocity_secs: u64,
}

pub async fn evaluate(_target: &MigrationTarget) -> HeliusFilterResult {
    // Recovered shadow implementation. Full server version used Helius account/holder lookups.
    HeliusFilterResult {
        passed: true,
        reason: "shadow_recovered_no_external_lookup".into(),
        creator_score: 0.0,
        top10_holder_pct: 0.0,
        curve_velocity_secs: 0,
    }
}
