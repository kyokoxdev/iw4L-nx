//! Campaign level hands: the `setViewmodel` literal the level gives its player.
//! Fork-only; kept out of match_walk.rs so upstream merges touch it as little as possible.

/// The hands model every FPV weapon of a campaign level is prepared with. The
/// level's own script first, then the loadout branch for `level.script == zone`.
// ponytail: one hands per level. Runtime `setviewmodel` to another name is
// ignored; switching live needs per-hands FPV prep.
pub(super) fn viewhands(scripts: &crate::ScriptSources, zone: &str) -> Option<String> {
    if !asset_transport::campaign_zone_set() {
        return None;
    }
    let text = |module: &str| {
        scripts
            .read(module)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
    };
    text(&format!("maps/{zone}"))
        .and_then(|main| viewmodel_literal(&main))
        .or_else(|| {
            let loadout = text("maps/_loadout")?;
            viewmodel_literal(level_block(&loadout, zone)?)
        })
}

/// The `{ ... }` body of `if ( level.script == "<zone>" )`.
fn level_block<'a>(text: &'a str, zone: &str) -> Option<&'a str> {
    let needle = format!("\"{zone}\"");
    for (at, _) in text.match_indices(&needle) {
        let before = text[..at].trim_end();
        let Some(before) = before.strip_suffix("==") else {
            continue;
        };
        if !before.trim_end().ends_with("level.script") {
            continue;
        }
        let body = &text[at..];
        let open = at + body.find('{')?;
        let mut depth = 0usize;
        for (i, c) in text[open..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(&text[open..=open + i]);
                    }
                }
                _ => {}
            }
        }
        return None;
    }
    None
}

/// The first string literal passed to `setViewmodel( "..." )`.
fn viewmodel_literal(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    for (at, _) in lower.match_indices("setviewmodel") {
        let rest = text[at + "setviewmodel".len()..].trim_start();
        let Some(rest) = rest.strip_prefix('(') else {
            continue;
        };
        let Some(rest) = rest.trim_start().strip_prefix('"') else {
            continue;
        };
        if let Some(end) = rest.find('"') {
            return Some(rest[..end].to_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_the_literal_of_the_zones_branch() {
        let text = "if ( level.script == \"pmc_x\" )\n{\n\tlevel.player setViewmodel( \"viewmodel_base_viewhands\" );\n}\n\
            if ( level.script == \"arcadia\" )\n{\n\tlevel.player setViewmodel( \"viewhands_airport\" );\n\treturn;\n}\n";
        let block = level_block(text, "arcadia").expect("arcadia branch");
        assert_eq!(viewmodel_literal(block).as_deref(), Some("viewhands_airport"));
        assert_eq!(level_block(text, "airport"), None);
        assert_eq!(viewmodel_literal("p getViewmodel(); p setviewmodeldepthoffield( \"x\" )"), None);
    }
}
