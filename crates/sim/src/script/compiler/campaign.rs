//! `iw4l check-scripts`: every link failure at once, for campaign script
//! sweeps. Fork-only; kept out of the parent file so upstream merges touch it
//! as little as possible.

use super::{Catalog, Fault, SourceResolver, link, parse};
use std::collections::{BTreeMap, BTreeSet};

/// Every link failure in the program, by message, with the modules it came
/// from. Parsing still stops at the first fault; linking does not.
pub(in crate::script) fn unresolved(
    resolver: &impl SourceResolver,
    roots: &[&str],
    catalog: &Catalog,
) -> Result<BTreeMap<String, BTreeSet<String>>, Fault> {
    let parsed = parse(resolver, roots, catalog)?;
    let mut failures: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for pending in &parsed.tables.pending {
        if let Err(message) = link(pending, &parsed.names, &parsed.imports, catalog) {
            failures
                .entry(message)
                .or_default()
                .insert(pending.module.clone());
        }
    }
    Ok(failures)
}

#[cfg(test)]
mod unresolved_tests {
    use super::*;

    #[test]
    fn unresolved_lists_every_unknown_builtin_without_stopping() {
        let sources = BTreeMap::from([(
            "maps/test".to_owned(),
            "main()\n{\n    x = abs(1);\n    self wave_flag(1);\n    spawn_thing();\n    helper();\n}\n\nhelper()\n{\n}\n".to_owned(),
        )]);
        let failures = unresolved(&sources, &["maps/test"], &Catalog::iw4_sp()).unwrap();
        let messages: Vec<&str> = failures.keys().map(String::as_str).collect();
        assert_eq!(
            messages,
            [
                "unresolved function or unknown builtin spawn_thing",
                "unresolved method or unknown builtin wave_flag",
            ]
        );
        assert!(
            failures
                .values()
                .all(|modules| modules.contains("maps/test"))
        );
    }
}
