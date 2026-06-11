#!/bin/bash
# Canary Checkpoint - pełny raport decyzyjny
# Zero live, zero restart, zero .env changes

set -e
cd /opt/huragan_core

echo "=== CANARY CHECKPOINT $(date '+%Y-%m-%d %H:%M:%S') ==="
echo ""

echo "--- Runtime Verify ---"
/opt/huragan_core/scripts/huragan_runtime_verify.sh 2>/dev/null
echo ""

echo "--- Z3 Outcome Audit ---"
python3 scripts/z3_outcome_audit.py 2>&1
echo ""

echo "--- Reserve Bucket Report ---"
python3 scripts/reserve_bucket_report.py 2>&1
echo ""

echo "--- Strategy Advisor ---"
python3 scripts/strategy_advisor.py 2>&1
echo ""
echo "=== END CHECKPOINT ==="
