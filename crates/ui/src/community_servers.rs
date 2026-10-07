use bevy::prelude::*;

// Switch: no updater (reqwest/tokio net); the list stays empty.
#[cfg(target_os = "horizon")]
mod updater {
    use std::path::{Path, PathBuf};

    pub struct CommunityFile {
        pub name: String,
        pub path: PathBuf,
    }

    pub fn communities() -> &'static [CommunityFile] {
        &[]
    }

    pub fn selected_path() -> Option<&'static Path> {
        None
    }

    pub fn restart_with_community(_: &Path) -> Result<(), String> {
        Err("Community servers are unavailable on Switch.".into())
    }
}

#[derive(Resource)]
pub struct CommunityServers {
    pub choices: Vec<(String, String)>,
    pub selected: String,
}

impl Default for CommunityServers {
    fn default() -> Self {
        let entries = updater::communities();
        let selected = entries
            .iter()
            .position(|entry| Some(entry.path.as_path()) == updater::selected_path());
        let mut choices: Vec<_> = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let name = if entries
                    .iter()
                    .filter(|other| other.name == entry.name)
                    .count()
                    > 1
                {
                    format!(
                        "{} ({})",
                        entry.name,
                        entry.path.file_name().map_or_else(
                            || entry.path.display().to_string(),
                            |name| name.to_string_lossy().into_owned()
                        )
                    )
                } else {
                    entry.name.clone()
                };
                (name, index.to_string())
            })
            .collect();
        if selected.is_none() {
            choices.insert(
                0,
                (
                    if entries.is_empty() {
                        "No community servers found"
                    } else {
                        "Choose a server"
                    }
                    .into(),
                    String::new(),
                ),
            );
        }
        Self {
            choices,
            selected: selected.map_or_else(String::new, |index| index.to_string()),
        }
    }
}

impl CommunityServers {
    pub fn restart(&self, choice: &str) -> Result<(), String> {
        let entry = choice
            .parse::<usize>()
            .ok()
            .and_then(|index| updater::communities().get(index))
            .ok_or("Choose a community server first.")?;
        updater::restart_with_community(&entry.path)
    }
}
