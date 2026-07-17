//! Shared parsing contract for small, case-insensitive CLI/marker argument
//! enums (`Shell`, `Target`, `Role`, `Domain`, ...).
//!
//! Each of these enums has historically hand-rolled the same normalization
//! boilerplate at the top of its `parse`: `s.trim().to_ascii_lowercase()`
//! before matching variant strings (and aliases). That normalization *policy*
//! — trim surrounding whitespace, fold to ASCII-lowercase, then match — is a
//! single cohesive contract; duplicating it per-enum let the policy drift.
//! [`ArgEnum`] owns it once: implementors provide only their own
//! variant/alias matching via [`ArgEnum::from_normalized`], and inherit a
//! `parse` that applies the shared normalization before delegating.

/// A CLI/marker argument enum parsed case-insensitively (trim + ascii-lowercase).
pub trait ArgEnum: Sized {
    /// Match an ALREADY-normalized token (trimmed, ascii-lowercased) to a
    /// variant. Implementors list only their own arms/aliases here.
    fn from_normalized(token: &str) -> Option<Self>;

    /// Parse raw user input: normalize once (trim + ascii-lowercase), then
    /// match. Callers use THIS, not `from_normalized` directly.
    fn parse(s: &str) -> Option<Self> {
        Self::from_normalized(s.trim().to_ascii_lowercase().as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hook::Shell;
    use crate::mode_apply::Target;
    use crate::modes::{Domain, Role};

    #[test]
    fn parse_normalizes_whitespace_and_case_shell() {
        assert_eq!(Shell::parse("  BASH "), Some(Shell::Bash));
    }

    #[test]
    fn parse_normalizes_whitespace_and_case_target() {
        assert_eq!(Target::parse("  Claude "), Some(Target::ClaudeCode));
    }

    #[test]
    fn parse_unknown_token_yields_none() {
        assert_eq!(Shell::parse("powershell"), None);
        assert_eq!(Target::parse("bogus"), None);
        assert_eq!(Role::parse("bogus"), None);
        assert_eq!(Domain::parse("bogus"), None);
    }

    #[test]
    fn from_normalized_accepts_already_normalized_token() {
        // Bypasses `parse`'s trim+lowercase step entirely: the input is
        // already in normalized form, so `from_normalized` must match it
        // directly without re-normalizing.
        assert_eq!(Shell::from_normalized("zsh"), Some(Shell::Zsh));
    }

    #[test]
    fn one_alias_per_enum_still_resolves() {
        assert_eq!(Target::parse("claude-code"), Some(Target::ClaudeCode));
        assert_eq!(Domain::parse("frontend"), Some(Domain::Web));
        assert_eq!(Role::parse("pm"), Some(Role::ProductManager));
    }
}
