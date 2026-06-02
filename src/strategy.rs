#[derive(Debug, Clone)]
pub struct StrategyVariant {
    pub id: &'static str,
    pub take_profit_ratio: f64,
    pub partial_sell_bps: u64,
    pub stop_loss_ratio: f64,
    pub trailing_stop_pct: f64,
    pub trailing_activation_ratio: f64,
    pub max_hold_secs: u64,
    pub early_no_momentum_secs: u64,
    pub early_no_momentum_min_ratio: f64,
    pub rug_guard_drawdown_pct: f64,
    pub rug_guard_requires_mfe_below_pct: f64,
    pub breakeven_floor_after_mfe_pct: f64,
}

impl StrategyVariant {
    pub fn f() -> Self {
        Self {
            id: "F",
            take_profit_ratio: 1.30,
            partial_sell_bps: 8000,
            stop_loss_ratio: 0.0,
            trailing_stop_pct: 0.0,
            trailing_activation_ratio: 1.10,
            max_hold_secs: 120,
            early_no_momentum_secs: 0,
            early_no_momentum_min_ratio: 0.0,
            rug_guard_drawdown_pct: 0.0,
            rug_guard_requires_mfe_below_pct: 0.0,
            breakeven_floor_after_mfe_pct: 0.0,
        }
    }

    pub fn i() -> Self {
        Self {
            id: "I",
            take_profit_ratio: 1.30,
            partial_sell_bps: 8000,
            stop_loss_ratio: 0.0,
            trailing_stop_pct: 0.0,
            trailing_activation_ratio: 1.10,
            max_hold_secs: 90,
            early_no_momentum_secs: 0,
            early_no_momentum_min_ratio: 0.0,
            rug_guard_drawdown_pct: 0.0,
            rug_guard_requires_mfe_below_pct: 0.0,
            breakeven_floor_after_mfe_pct: 0.0,
        }
    }

    pub fn z() -> Self {
        Self {
            id: "Z",
            take_profit_ratio: 9_999.0,
            partial_sell_bps: 0,
            stop_loss_ratio: 0.0,
            trailing_stop_pct: 15.0,
            trailing_activation_ratio: 1.10,
            max_hold_secs: 300,
            early_no_momentum_secs: 0,
            early_no_momentum_min_ratio: 0.0,
            rug_guard_drawdown_pct: 0.0,
            rug_guard_requires_mfe_below_pct: 0.0,
            breakeven_floor_after_mfe_pct: 0.0,
        }
    }

    pub fn z2() -> Self {
        Self {
            id: "Z2",
            take_profit_ratio: 9_999.0,
            partial_sell_bps: 0,
            stop_loss_ratio: 0.0,
            trailing_stop_pct: 15.0,
            trailing_activation_ratio: 1.10,
            max_hold_secs: 180,
            early_no_momentum_secs: 60,
            early_no_momentum_min_ratio: 1.05,
            rug_guard_drawdown_pct: 35.0,
            rug_guard_requires_mfe_below_pct: 10.0,
            breakeven_floor_after_mfe_pct: 0.0,
        }
    }

    pub fn z3() -> Self {
        Self {
            id: "Z3",
            take_profit_ratio: 9_999.0,
            partial_sell_bps: 0,
            stop_loss_ratio: 0.80,
            trailing_stop_pct: 0.0,
            trailing_activation_ratio: 1.05,
            max_hold_secs: 300,
            early_no_momentum_secs: 0,
            early_no_momentum_min_ratio: 0.0,
            rug_guard_drawdown_pct: 0.0,
            rug_guard_requires_mfe_below_pct: 0.0,
            breakeven_floor_after_mfe_pct: 20.0,
        }
    }

    /// Z4 — deprecated. Retained for historical state.jsonl compatibility.
    pub fn z4() -> Self {
        Self {
            id: "Z4",
            take_profit_ratio: 1.30,
            partial_sell_bps: 5000,
            stop_loss_ratio: 0.0,
            trailing_stop_pct: 15.0,
            trailing_activation_ratio: 1.10,
            max_hold_secs: 300,
            early_no_momentum_secs: 0,
            early_no_momentum_min_ratio: 0.0,
            rug_guard_drawdown_pct: 0.0,
            rug_guard_requires_mfe_below_pct: 0.0,
            breakeven_floor_after_mfe_pct: 0.0,
        }
    }

    /// Z3.1 — Z3 with shorter max_hold (210s instead of 240s)
    pub fn z31() -> Self {
        Self {
            id: "Z3.1",
            take_profit_ratio: 9_999.0,
            partial_sell_bps: 0,
            stop_loss_ratio: 0.0,
            trailing_stop_pct: 12.0,
            trailing_activation_ratio: 1.05,
            max_hold_secs: 210,
            early_no_momentum_secs: 0,
            early_no_momentum_min_ratio: 0.0,
            rug_guard_drawdown_pct: 0.0,
            rug_guard_requires_mfe_below_pct: 0.0,
            breakeven_floor_after_mfe_pct: 20.0,
        }
    }
}

pub struct StrategyEvaluator {
    variants: Vec<StrategyVariant>,
}

impl StrategyEvaluator {
    pub fn new() -> Self {
        Self {
            variants: vec![
                StrategyVariant::f(),
                StrategyVariant::i(),
                StrategyVariant::z(),
                StrategyVariant::z2(),
                StrategyVariant::z3(),
                StrategyVariant::z31(),
            ],
        }
    }

    pub fn variants(&self) -> &[StrategyVariant] {
        &self.variants
    }

    pub fn variant(&self, id: &str) -> Option<StrategyVariant> {
        self.variants.iter().find(|v| v.id == id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::{StrategyEvaluator, StrategyVariant};

    #[test]
    fn evaluator_loads_active_shadow_variants() {
        let evaluator = StrategyEvaluator::new();
        let ids: Vec<_> = evaluator.variants().iter().map(|v| v.id).collect();
        assert_eq!(ids, vec!["F", "I", "Z", "Z2", "Z3", "Z3.1"]);
    }

    #[test]
    fn z_baseline_and_shadow_params_are_stable() {
        let z = StrategyVariant::z();
        assert_eq!(z.max_hold_secs, 300);
        assert_eq!(z.trailing_stop_pct, 15.0);
        assert_eq!(z.trailing_activation_ratio, 1.10);

        let z2 = StrategyVariant::z2();
        assert_eq!(z2.max_hold_secs, 180);
        assert_eq!(z2.early_no_momentum_secs, 60);
        assert_eq!(z2.early_no_momentum_min_ratio, 1.05);
        assert_eq!(z2.rug_guard_drawdown_pct, 35.0);

        let z3 = StrategyVariant::z3();
        assert_eq!(z3.trailing_activation_ratio, 1.05);
        assert_eq!(z3.stop_loss_ratio, 0.80);
        assert_eq!(z3.trailing_stop_pct, 0.0);
        assert_eq!(z3.max_hold_secs, 300);
        assert_eq!(z3.breakeven_floor_after_mfe_pct, 20.0);

        let z31 = StrategyVariant::z31();
        assert_eq!(z31.max_hold_secs, 210);
        assert_eq!(z31.trailing_activation_ratio, 1.05);
        assert_eq!(z31.breakeven_floor_after_mfe_pct, 20.0);
    }
}
