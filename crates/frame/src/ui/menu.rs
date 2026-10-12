use bevy::prelude::*;

#[derive(Resource, Default, Debug)]
pub struct UiPartyState {
    pub active: bool,
    pub in_lobby: bool,
    pub is_host: bool,
}

#[derive(Resource, Default, Debug)]
pub struct UiMenuDvars {
    values: std::collections::HashMap<String, String>,
}

impl UiMenuDvars {
    // Hot (menus read and republish dvars every frame): only allocate a key
    // when the name has uppercase or is new.
    fn key(name: &str) -> std::borrow::Cow<'_, str> {
        if name.bytes().any(|b| b.is_ascii_uppercase()) {
            name.to_ascii_lowercase().into()
        } else {
            name.into()
        }
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.values.get(Self::key(name).as_ref()).map(String::as_str)
    }

    pub fn set(&mut self, name: &str, value: impl Into<String>) {
        let key = Self::key(name);
        match self.values.get_mut(key.as_ref()) {
            Some(current) => *current = value.into(),
            None => {
                self.values.insert(key.into_owned(), value.into());
            }
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.values
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
    }
}

#[derive(Resource, Default, Debug)]
pub struct FrontendContentLoading;

#[derive(Resource, Clone, Default, Debug)]
pub struct HostMatchRules(pub Vec<(String, String)>);

#[derive(Message, Clone, Debug)]
pub struct UiExecCommand {
    pub text: String,
}

#[derive(Message, Clone, Debug, PartialEq, Eq)]
pub enum UiMenuRequest {
    Toggle,
    RestoreLobby,
    Open(String),
    Close(String),
    Focus { menu: String, item: String },
    Key(UiMenuKey),
    Text(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiMenuKey {
    Escape,
    Enter,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    Backspace,
    Delete,
}
