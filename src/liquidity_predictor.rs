use crate::engine::MigrationTarget;

#[derive(Debug, Clone, Default)]
pub struct LiquidityPrediction {
    pub creator_score: f64,
    pub top10_holder_pct: f64,
    pub curve_velocity_secs: u64,
    pub safety_score_passed: bool,
    pub reason: String,
}

pub async fn score(target: &MigrationTarget) -> LiquidityPrediction {
    let creator_score = target.creator_score;
    let top10 = target.top10_holder_pct;
    let velocity = target.curve_velocity_secs;
    let passed = creator_score >= 0.20
        && (top10 == 0.0 || top10 <= 0.35)
        && (velocity == 0 || velocity >= 45);
    LiquidityPrediction {
        creator_score,
        top10_holder_pct: top10,
        curve_velocity_secs: velocity,
        safety_score_passed: passed,
        reason: if passed {
            "predictor_passed"
        } else {
            "predictor_shadow_risk"
        }
        .into(),
    }
}
