//! Privacy helpers — redact account identifiers in outputs.

use crate::providers::types::ProviderSnapshot;

/// Redact an email or account identifier.
///
/// `user@domain.com` → `u***@domain.com`
/// short / non-email → `***`
pub fn redact_account(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if let Some((local, domain)) = trimmed.split_once('@') {
        if !domain.is_empty() {
            let first = local.chars().next().unwrap_or('*');
            return format!("{first}***@{domain}");
        }
    }
    if trimmed.chars().count() <= 3 {
        return "***".into();
    }
    let first = trimmed.chars().next().unwrap_or('*');
    format!("{first}***")
}

/// Apply privacy redaction to snapshots (in place).
pub fn redact_snapshots(snaps: &mut [ProviderSnapshot]) {
    for snap in snaps {
        if let Some(account) = snap.account.as_mut() {
            *account = redact_account(account);
        }
    }
}

pub fn maybe_redact(account: Option<&str>, privacy: bool) -> Option<String> {
    account.map(|a| {
        if privacy {
            redact_account(a)
        } else {
            a.to_string()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_email() {
        assert_eq!(redact_account("alice@example.com"), "a***@example.com");
    }

    #[test]
    fn redacts_short() {
        assert_eq!(redact_account("ab"), "***");
    }
}
