# TASK_07 Price Quote Validator Implementation Report

Mode: **PAPER/VALIDATION ONLY**. No live, no real buy/sell, no policy change, no restart, no TASK_08.

## Summary

- total_completed: **7186**
- quote_valid_count: **6289**
- quote_unavailable_count: **655**
- quote_invalid_count: **93**
- quote_artifact_count: **149**
- valuation_uncertain_count: **897**
- exit_affected_count: **257**
- metrics_eligible_count: **6944**
- exit_design_eligible_count: **5851**
- price_unavailable_profitable_count: **398**
- price_unavailable_exit_affected_count: **257**
- native_validator_rows_seen: **0**

## Metrics Eligible

- count: 6944
- wr: 4815/6944
- avg_pnl_pct: 14.528811
- median_pnl_pct: 8.500133

## Exit Design Eligible

- count: 5851
- wr: 4417/5851
- avg_pnl_pct: 22.305465
- median_pnl_pct: 12.8919

## Example rows

| mint | variant | reason | pnl% | mfe% | valid | unavailable | invalid | artifact | exit_design | codes |
|---|---|---|---:|---:|---|---|---|---|---|---|
| AGJVUkyp | Z3 | max_hold | 57.016 | 65.525 | True | False | False | False | True | strong_runner_clean_path |
| 4F733e6v | Z3.1 | price_unavailable | 5.919 | 6.194 | False | True | False | False | False | price_unavailable_seen |
| 4UeXo6Ks | Z3.1 | price_unavailable | 2.151 | 10.737 | False | True | False | False | False | price_unavailable_seen |
| 4F733e6v | Z3 | price_unavailable | 5.919 | 6.162 | False | True | False | False | False | price_unavailable_seen |
| BjQNqFZ6 | Z3 | early_no_momentum | -4.266 | 0.775 | True | False | False | False | True | clean_trade |
| 4UeXo6Ks | Z3 | price_unavailable | 2.273 | 10.939 | False | True | False | False | False | price_unavailable_seen |
| GF9f9mj3 | Z3.1 | max_hold | 50.620 | 51.085 | True | False | False | False | True | strong_runner_clean_path |
| BjQNqFZ6 | Z3.1 | max_hold | -3.695 | 2.000 | True | False | False | False | True | clean_trade |
| GF9f9mj3 | Z3 | max_hold | 54.105 | 56.673 | True | False | False | False | True | strong_runner_clean_path |
| EsPZtqFN | Z3 | early_no_momentum | -1.459 | 4.779 | True | False | False | False | True | clean_trade |
| AUdrPdiS | Z3.1 | price_unavailable | 1.655 | 7.983 | False | True | False | False | False | price_unavailable_seen |
| EsPZtqFN | Z3.1 | max_hold | -0.890 | 5.023 | True | False | False | False | True | clean_trade |
| AUdrPdiS | Z3 | price_unavailable | 1.632 | 7.983 | False | True | False | False | False | price_unavailable_seen |
| FzDuecMX | Z3 | controlled_pump_exit | 18.435 | 27.928 | True | False | False | False | True | strong_runner_clean_path |
| DMSgW3Fv | Z3 | early_no_momentum | -5.202 | 0.000 | True | False | False | False | True | clean_trade |
| 97TzaAtH | Z3 | early_no_momentum | 5.413 | 7.647 | True | False | False | False | True | clean_trade |
| DMSgW3Fv | Z3.1 | max_hold | -4.531 | 0.000 | True | False | False | False | True | clean_trade |
| 97TzaAtH | Z3.1 | max_hold | 10.065 | 12.405 | True | False | False | False | True | clean_trade |
| E1XjMAyt | Z3 | dead_momentum | 0.046 | 0.394 | True | False | False | False | True | clean_trade |
| CPy3Ht3D | Z3 | controlled_pump_exit | 16.727 | 26.201 | True | False | False | False | True | strong_runner_clean_path |
| 5na7LMdP | Z3 | max_hold | 7.389 | 17.242 | True | False | False | False | True | clean_trade |
| ABZCTXLT | Z3 | early_no_momentum | -0.540 | 2.610 | True | False | False | False | True | clean_trade |
| ABZCTXLT | Z3.1 | price_unavailable | 2.106 | 3.690 | False | True | False | False | False | price_unavailable_seen |
| Cz9r1PXf | Z3 | hard_stop | -45.346 | 0.000 | True | False | False | False | False | rug_like_terminal_loss |
| F4c52jjW | Z3.1 | max_hold | 40.307 | 42.714 | True | False | False | False | True | strong_runner_clean_path |
| BqiuXyxo | Z3.1 | price_unavailable | 26.516 | 30.354 | False | True | False | False | False | price_unavailable_seen |
| C6paN31V | Z3.1 | price_unavailable | 18.846 | 20.261 | False | True | False | False | False | price_unavailable_seen |
| F4c52jjW | Z3 | max_hold | 45.292 | 48.160 | True | False | False | False | True | strong_runner_clean_path |
| BqiuXyxo | Z3 | price_unavailable | 27.396 | 30.354 | False | True | False | False | False | price_unavailable_seen |
| Cz9r1PXf | Z3.1 | breakeven_floor | -0.333 | 44.125 | True | False | False | False | True | mfe_giveback_clean_path |
| C6paN31V | Z3 | price_unavailable | 18.768 | 19.712 | False | True | False | False | False | price_unavailable_seen |
| 2trpEGAB | Z3.1 | max_hold | 19.210 | 21.339 | True | False | False | False | True | clean_trade |
| 68RgvWW5 | Z3 | early_no_momentum | -5.926 | 0.000 | True | False | False | False | True | clean_trade |
| FqnsfrZ3 | Z3 | early_no_momentum | -3.465 | 0.000 | True | False | False | False | True | clean_trade |
| FAAcExGo | Z3.1 | price_unavailable | 4.576 | 5.474 | False | True | False | False | False | price_unavailable_seen |
| FqnsfrZ3 | Z3.1 | max_hold | 1.125 | 3.469 | True | False | False | False | True | clean_trade |
| 68RgvWW5 | Z3.1 | max_hold | -5.191 | 0.000 | True | False | False | False | True | clean_trade |
| Eh1fuZfy | Z3 | early_no_momentum | 7.436 | 7.770 | True | False | False | False | True | clean_trade |
| DRXeBVQJ | Z3 | early_no_momentum | 2.853 | 5.038 | True | False | False | False | True | clean_trade |
| FAAcExGo | Z3 | price_unavailable | 4.576 | 4.798 | False | True | False | False | False | price_unavailable_seen |

## Final guard

This report summarizes validation metadata only. No trading behavior changes.
