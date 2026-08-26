//! Shared parsing contract for small, case-insensitive CLI/marker argument
//! enums (`Target`, ...).
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
    use crate::mode_apply::Target;

    #[test]
    fn parse_normalizes_whitespace_and_case_target() {
        assert_eq!(Target::parse("  Claude "), Some(Target::ClaudeCode));
    }

    #[test]
    fn parse_unknown_token_yields_none() {
        assert_eq!(Target::parse("bogus"), None);
    }

    #[test]
    fn from_normalized_accepts_already_normalized_token() {
        // Bypasses `parse`'s trim+lowercase step entirely: the input is
        // already in normalized form, so `from_normalized` must match it
        // directly without re-normalizing.
        assert_eq!(Target::from_normalized("grok"), Some(Target::Grok));
    }

    #[test]
    fn one_alias_per_enum_still_resolves() {
        assert_eq!(Target::parse("claude-code"), Some(Target::ClaudeCode));
    }

    // -- Round-1 edge-case hunt: shared normalization contract -------------

    #[test]
    fn parse_trims_tabs_and_newlines_not_just_spaces() {
        // `.trim()` strips all ASCII (and Unicode) whitespace, not just the
        // plain space char — tabs/newlines must normalize the same way.
        assert_eq!(Target::parse("\t grok \n"), Some(Target::Grok));
    }

    #[test]
    fn parse_does_not_strip_interior_whitespace() {
        // Normalization is trim + case-fold ONLY. A space is not an alias
        // separator: "claude code" must NOT resolve like "claude-code" does.
        assert_eq!(Target::parse("claude code"), None);
    }

    #[test]
    fn parse_case_folds_aliases_across_all_enums() {
        // Case-folding must reach every enum's ALIAS arms, not just its
        // primary variant name — one representative alias per enum.
        assert_eq!(Target::parse("CLAUDE-CODE"), Some(Target::ClaudeCode));
    }

    #[test]
    fn parse_empty_and_whitespace_only_yields_none() {
        assert_eq!(Target::parse(""), None);
        assert_eq!(Target::parse("   "), None);
    }

    #[test]
    fn parse_does_not_fold_non_ascii_case() {
        // Documented ascii-only contract: `to_ascii_lowercase` leaves
        // non-ASCII letters untouched, so a non-ASCII-cased token never
        // accidentally matches a variant even under full Unicode folding.
        assert_eq!(Target::parse("GRÖK"), None);
    }

    #[test]
    fn from_normalized_does_not_re_normalize_uppercase_input() {
        // `from_normalized` trusts its caller (`parse`) to have already
        // normalized; fed raw uppercase input directly, it must NOT match —
        // proving normalization is `parse`'s job alone, never duplicated.
        assert_eq!(Target::from_normalized("GROK"), None);
    }

    #[test]
    fn inherent_parse_matches_trait_parse_for_sample_inputs() {
        // The inherent `T::parse` on each enum must delegate faithfully to
        // `<T as ArgEnum>::parse` — never hand-roll a diverging check.
        for input in ["claude", "GROK", "bogus"] {
            assert_eq!(Target::parse(input), <Target as ArgEnum>::parse(input));
        }
    }
}
