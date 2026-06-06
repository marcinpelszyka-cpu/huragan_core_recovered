use crate::engine::MigrationTarget;

pub fn static_filter(target: &MigrationTarget) -> Result<(), String> {
    let name = target.name.trim();
    let symbol = target.symbol.trim();
    if name.len() < 2 || symbol.is_empty() {
        return Err("name_or_symbol_too_short".into());
    }
    if name.len() > 32 || symbol.len() > 16 {
        return Err("name_or_symbol_too_long".into());
    }
    if name.chars().filter(|c| c.is_ascii_uppercase()).count() > 18 {
        return Err("caps_lock_pattern".into());
    }
    if name.eq_ignore_ascii_case(symbol) && name.len() > 8 {
        return Err("same_name_symbol_spam".into());
    }
    Ok(())
}
