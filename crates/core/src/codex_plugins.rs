//! Codex CLI plugin remove — remove-only. No list, add, enable, or doctor.

use crate::runner::Invocation;
use std::path::Path;

/// `codex plugin remove <spec>` — uninstall one plugin. `spec` is a single
/// argv slot (do not split on `@`).
pub fn remove_invocation(spec: &str, cwd: &Path) -> Invocation {
    Invocation::new("codex", &["plugin", "remove", spec], cwd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn invocation_shapes() {
        let inv = remove_invocation("sample@debug", Path::new("/cwd"));
        assert_eq!(inv.program, "codex");
        assert_eq!(inv.args, ["plugin", "remove", "sample@debug"]);
        assert_eq!(inv.display(), "codex plugin remove sample@debug");
    }
}
