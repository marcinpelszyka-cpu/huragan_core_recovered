use crate::engine::{MigrationTarget, SPL_TOKEN_2022_PROGRAM, SPL_TOKEN_PROGRAM, WSOL_MINT};
use serde::{Deserialize, Serialize};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use spl_token::solana_program::program_option::COption;
use spl_token::solana_program::program_pack::Pack;
use spl_token_2022::extension::BaseStateWithExtensions;
use std::str::FromStr;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct MintAuthorityAudit {
    pub passed: bool,
    pub reason: String,
    pub audited_mint: String,
    pub token_program_owner: String,
    pub token_program_kind: String,
    pub mint_initialized: bool,
    pub mint_supply: u64,
    pub mint_decimals: u8,
    pub mint_authority_present: bool,
    pub mint_authority: String,
    pub freeze_authority_present: bool,
    pub freeze_authority: String,
    pub token2022_extensions: Vec<String>,
}

impl MintAuthorityAudit {
    #[allow(dead_code)]
    pub fn blocked(audited_mint: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            passed: false,
            reason: reason.into(),
            audited_mint: audited_mint.into(),
            ..Default::default()
        }
    }

    pub fn pass_reason() -> &'static str {
        "mint_authority_audit_passed"
    }
}

pub fn expected_coin_mint(target: &MigrationTarget) -> anyhow::Result<String> {
    if target.base_mint == WSOL_MINT && !target.quote_mint.is_empty() {
        return Ok(target.quote_mint.clone());
    }
    if target.quote_mint == WSOL_MINT && !target.base_mint.is_empty() {
        return Ok(target.base_mint.clone());
    }
    if !target.quote_asset_mint.is_empty() {
        if target.base_mint == target.quote_asset_mint && !target.quote_mint.is_empty() {
            return Ok(target.quote_mint.clone());
        }
        if target.quote_mint == target.quote_asset_mint && !target.base_mint.is_empty() {
            return Ok(target.base_mint.clone());
        }
    }
    if !target.mint.is_empty() {
        return Ok(target.mint.clone());
    }
    anyhow::bail!("mint_missing")
}

pub fn validate_target_mint(target: &MigrationTarget) -> anyhow::Result<String> {
    let expected = expected_coin_mint(target)?;
    if !target.mint.is_empty() && target.mint != expected {
        anyhow::bail!("mint_mismatch:{}!={}", target.mint, expected);
    }
    Ok(expected)
}

pub async fn audit_target_mint(
    rpc: &RpcClient,
    target: &MigrationTarget,
) -> anyhow::Result<MintAuthorityAudit> {
    let audited_mint = validate_target_mint(target)?;
    let mint_pubkey = Pubkey::from_str(&audited_mint)?;
    let account = rpc.get_account(&mint_pubkey).await?;
    audit_mint_account(&audited_mint, &account.owner.to_string(), &account.data)
}

pub fn audit_mint_account(
    audited_mint: &str,
    owner: &str,
    data: &[u8],
) -> anyhow::Result<MintAuthorityAudit> {
    if owner == SPL_TOKEN_PROGRAM {
        let mint = spl_token::state::Mint::unpack(data)?;
        return Ok(decide_spl_mint(audited_mint, owner, mint));
    }

    if owner == SPL_TOKEN_2022_PROGRAM {
        let mut audit = MintAuthorityAudit {
            passed: false,
            reason: "token2022_blocked".into(),
            audited_mint: audited_mint.into(),
            token_program_owner: owner.into(),
            token_program_kind: "token2022".into(),
            ..Default::default()
        };
        if let Ok(state) = spl_token_2022::extension::StateWithExtensions::<
            spl_token_2022::state::Mint,
        >::unpack(data)
        {
            audit.mint_initialized = state.base.is_initialized;
            audit.mint_supply = state.base.supply;
            audit.mint_decimals = state.base.decimals;
            fill_authorities_2022(
                &mut audit,
                state.base.mint_authority,
                state.base.freeze_authority,
            );
            if let Ok(exts) = state.get_extension_types() {
                audit.token2022_extensions = exts.iter().map(|e| format!("{e:?}")).collect();
            }
            if audit.mint_authority_present {
                audit.reason = format!("mint_authority_present:{}", audit.mint_authority);
            } else if audit.freeze_authority_present {
                audit.reason = format!("freeze_authority_present:{}", audit.freeze_authority);
            } else if !audit.token2022_extensions.is_empty() {
                audit.reason =
                    format!("token2022_blocked:{}", audit.token2022_extensions.join(","));
            }
        }
        return Ok(audit);
    }

    Ok(MintAuthorityAudit {
        passed: false,
        reason: format!("mint_account_owner_unsupported:{owner}"),
        audited_mint: audited_mint.into(),
        token_program_owner: owner.into(),
        token_program_kind: "unsupported".into(),
        ..Default::default()
    })
}

fn decide_spl_mint(
    audited_mint: &str,
    owner: &str,
    mint: spl_token::state::Mint,
) -> MintAuthorityAudit {
    let mut audit = MintAuthorityAudit {
        passed: false,
        reason: String::new(),
        audited_mint: audited_mint.into(),
        token_program_owner: owner.into(),
        token_program_kind: "spl-token".into(),
        mint_initialized: mint.is_initialized,
        mint_supply: mint.supply,
        mint_decimals: mint.decimals,
        ..Default::default()
    };
    fill_authorities(&mut audit, mint.mint_authority, mint.freeze_authority);
    audit.reason = if !audit.mint_initialized {
        "mint_uninitialized".into()
    } else if audit.mint_authority_present {
        format!("mint_authority_present:{}", audit.mint_authority)
    } else if audit.freeze_authority_present {
        format!("freeze_authority_present:{}", audit.freeze_authority)
    } else {
        audit.passed = true;
        MintAuthorityAudit::pass_reason().into()
    };
    audit
}

fn fill_authorities(
    audit: &mut MintAuthorityAudit,
    mint_authority: COption<Pubkey>,
    freeze_authority: COption<Pubkey>,
) {
    match mint_authority {
        COption::Some(pk) => {
            audit.mint_authority_present = true;
            audit.mint_authority = pk.to_string();
        }
        COption::None => {}
    }
    match freeze_authority {
        COption::Some(pk) => {
            audit.freeze_authority_present = true;
            audit.freeze_authority = pk.to_string();
        }
        COption::None => {}
    }
}

fn fill_authorities_2022(
    audit: &mut MintAuthorityAudit,
    mint_authority: spl_token_2022::solana_program::program_option::COption<Pubkey>,
    freeze_authority: spl_token_2022::solana_program::program_option::COption<Pubkey>,
) {
    match mint_authority {
        spl_token_2022::solana_program::program_option::COption::Some(pk) => {
            audit.mint_authority_present = true;
            audit.mint_authority = pk.to_string();
        }
        spl_token_2022::solana_program::program_option::COption::None => {}
    }
    match freeze_authority {
        spl_token_2022::solana_program::program_option::COption::Some(pk) => {
            audit.freeze_authority_present = true;
            audit.freeze_authority = pk.to_string();
        }
        spl_token_2022::solana_program::program_option::COption::None => {}
    }
}

pub fn fill_state_fields(state: &mut crate::state::PositionState, audit: &MintAuthorityAudit) {
    // Fields not yet added to PositionState — stub for future integration.
    // The exit_reason field on PositionState already captures the audit result
    // (e.g., "mint_authority_present:..." or "freeze_authority_present:...").
    let _ = (state, audit);
}

#[cfg(test)]
mod tests {
    use super::*;
    use spl_token::solana_program::program_option::COption;
    use spl_token::solana_program::program_pack::Pack;

    fn target(base: &str, quote: &str, mint: &str) -> MigrationTarget {
        MigrationTarget {
            base_mint: base.into(),
            quote_mint: quote.into(),
            quote_asset_mint: WSOL_MINT.into(),
            mint: mint.into(),
            source: "helius_migration".into(),
            pool_state: "pool".into(),
            ..Default::default()
        }
    }

    fn mint_data(
        mint_auth: COption<Pubkey>,
        freeze_auth: COption<Pubkey>,
        initialized: bool,
    ) -> Vec<u8> {
        let mint = spl_token::state::Mint {
            mint_authority: mint_auth,
            supply: 1_000_000,
            decimals: 6,
            is_initialized: initialized,
            freeze_authority: freeze_auth,
        };
        let mut data = vec![0u8; spl_token::state::Mint::LEN];
        spl_token::state::Mint::pack(mint, &mut data).unwrap();
        data
    }

    #[test]
    fn expected_coin_mint_handles_wsol_base_and_quote() {
        let coin = "Coin111111111111111111111111111111111111111";
        assert_eq!(
            expected_coin_mint(&target(WSOL_MINT, coin, coin)).unwrap(),
            coin
        );
        assert_eq!(
            expected_coin_mint(&target(coin, WSOL_MINT, coin)).unwrap(),
            coin
        );
    }

    #[test]
    fn validate_target_mint_blocks_pool_mismatch() {
        let coin = "Coin111111111111111111111111111111111111111";
        let other = "Other11111111111111111111111111111111111111";
        let err = validate_target_mint(&target(WSOL_MINT, coin, other))
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("mint_mismatch:"));
    }

    #[test]
    fn spl_mint_without_authorities_passes() {
        let data = mint_data(COption::None, COption::None, true);
        let audit = audit_mint_account("coin", SPL_TOKEN_PROGRAM, &data).unwrap();
        assert!(audit.passed);
        assert_eq!(audit.reason, "mint_authority_audit_passed");
        assert_eq!(audit.token_program_kind, "spl-token");
    }

    #[test]
    fn spl_mint_with_mint_authority_blocks() {
        let auth = Pubkey::new_unique();
        let data = mint_data(COption::Some(auth), COption::None, true);
        let audit = audit_mint_account("coin", SPL_TOKEN_PROGRAM, &data).unwrap();
        assert!(!audit.passed);
        assert!(audit.reason.starts_with("mint_authority_present:"));
        assert!(audit.mint_authority_present);
    }

    #[test]
    fn spl_mint_with_freeze_authority_blocks() {
        let auth = Pubkey::new_unique();
        let data = mint_data(COption::None, COption::Some(auth), true);
        let audit = audit_mint_account("coin", SPL_TOKEN_PROGRAM, &data).unwrap();
        assert!(!audit.passed);
        assert!(audit.reason.starts_with("freeze_authority_present:"));
        assert!(audit.freeze_authority_present);
    }

    #[test]
    fn unsupported_owner_blocks() {
        let audit = audit_mint_account("coin", "11111111111111111111111111111111", &[]).unwrap();
        assert!(!audit.passed);
        assert!(audit.reason.starts_with("mint_account_owner_unsupported:"));
    }
}
