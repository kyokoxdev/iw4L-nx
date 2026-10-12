use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use bevy::prelude::*;
use session::LocalAccount;
use sim::{AccountId, AccountSnapshot, PLAYER_DATA_BUFFER_BYTES};

const MAGIC: &[u8; 8] = b"IW4LACC3";
const KEYED_MAGIC: &[u8; 8] = b"IW4LACC2";
const LEGACY_MAGIC: &[u8; 8] = b"IW4LACC1";
const LEGACY_EMPTY_BYTES: usize = 25;
const LEGACY_SNAPSHOT_BYTES: usize = LEGACY_EMPTY_BYTES + 16 + PLAYER_DATA_BUFFER_BYTES;
const EMPTY_BYTES: usize = 57;
const KEYED_SNAPSHOT_BYTES: usize = EMPTY_BYTES + 16 + PLAYER_DATA_BUFFER_BYTES;
const SNAPSHOT_BYTES: usize = KEYED_SNAPSHOT_BYTES + sim::SKILL_RATING_BYTES;
const SAVE_RETRY_DELAY: Duration = Duration::from_secs(5);

#[derive(Resource, Default)]
pub(crate) struct AccountPersistence {
    path: Option<PathBuf>,
    saved: Option<AccountSnapshot>,
    retry_at: Option<Instant>,
    last_error: Option<(io::ErrorKind, String)>,
    /// The write in flight: `sync_all` + rename on the Switch SD card cost tens
    /// of ms, and match scripts bump the stats every second.
    writing: Option<(AccountSnapshot, std::thread::JoinHandle<io::Result<()>>)>,
}

pub(crate) fn load(
    mut commands: Commands,
    identity: Res<ui::LaunchIdentity>,
    role: Res<frame::RuntimeRole>,
    mut persistence: ResMut<AccountPersistence>,
) {
    if !matches!(
        *role,
        frame::RuntimeRole::Listen | frame::RuntimeRole::Client
    ) {
        return;
    }
    let path = std::env::var_os("IW4L_ACCOUNT_PATH")
        .map(PathBuf::from)
        .or_else(|| {
            crate::user_settings::settings_path(&identity.artifacts)
                .map(|path| path.with_file_name("account.dat"))
        });
    let Some(path) = path else {
        warn!("no HOME or XDG_CONFIG_HOME; no durable local account available");
        return;
    };
    match load_or_create(&path) {
        Ok(account) => {
            persistence.path = Some(path);
            persistence.saved = account.snapshot.clone();
            commands.insert_resource(net::AccountSaveReceipt(account.snapshot.clone()));
            commands.insert_resource(account);
        }
        Err(error) => warn!("could not load local account {}: {error}", path.display()),
    }
}

pub(crate) fn save(
    role: Res<frame::RuntimeRole>,
    authority: Option<ResMut<net::AuthorityWorld>>,
    account: Option<ResMut<LocalAccount>>,
    mut persistence: ResMut<AccountPersistence>,
    mut receipt: Option<ResMut<net::AccountSaveReceipt>>,
    exiting: Option<Res<crate::feature_dispatch::PendingExit>>,
    mut player_data: ResMut<sim::LocalPlayerData>,
) {
    if !matches!(
        *role,
        frame::RuntimeRole::Listen | frame::RuntimeRole::Client
    ) {
        return;
    }
    let (Some(mut account), Some(path)) = (account, persistence.path.clone()) else {
        return;
    };
    let mut authority = authority;
    // The process leaves after this system: the last write must land first.
    let exiting = exiting.is_some();
    if persistence
        .writing
        .as_ref()
        .is_some_and(|(_, handle)| !handle.is_finished() && !exiting)
    {
        return;
    }
    if let Some((snapshot, handle)) = persistence.writing.take() {
        let result = handle
            .join()
            .unwrap_or_else(|_| Err(io::Error::other("account writer panicked")));
        finish_write(
            result,
            snapshot,
            *role,
            &path,
            &mut persistence,
            authority.as_deref_mut(),
            &mut account,
            receipt.as_deref_mut(),
        );
    }
    // A step in flight holds the world: leave the save to a frame without one,
    // except the last before exit, which waits for it. Steps start only on
    // this thread, so a world free here stays free for the rest of the system.
    if *role == frame::RuntimeRole::Listen
        && !exiting
        && authority
            .as_ref()
            .is_some_and(|authority| authority.try_lock().is_none())
    {
        return;
    }
    // Every frame: skip the account-buffer clone and compare once the live
    // revision is the one already saved and receipted.
    if *role == frame::RuntimeRole::Listen
        && let (Some(saved), Some(authority)) = (persistence.saved.as_ref(), authority.as_ref())
        && saved.account == account.id
        && authority.lock().persistent_data().revision(account.id) == Some(saved.revision)
        && receipt.as_ref().is_none_or(|receipt| {
            receipt
                .0
                .as_ref()
                .is_some_and(|r| r.account == saved.account && r.revision == saved.revision)
        })
    {
        return;
    }
    let snapshot = if *role == frame::RuntimeRole::Listen {
        authority
            .as_ref()
            .and_then(|authority| authority.lock().persistent_data().snapshot(account.id))
    } else {
        account.snapshot.clone()
    };
    let Some(snapshot) = snapshot else {
        return;
    };
    let _ = player_data.refresh(Some(&snapshot));
    if persistence.saved.as_ref() == Some(&snapshot) {
        persistence.retry_at = None;
        persistence.last_error = None;
        if let Some(receipt) = receipt.as_mut() {
            receipt.0 = Some(snapshot.clone());
        }
        if *role == frame::RuntimeRole::Listen
            && let Some(authority) = authority.as_mut()
        {
            authority
                .lock()
                .persistent_data_mut()
                .acknowledge_saved(account.id, snapshot.revision);
        }
        return;
    };
    if persistence
        .retry_at
        .is_some_and(|retry_at| Instant::now() < retry_at)
    {
        return;
    }

    let next = LocalAccount {
        id: account.id,
        key: account.key.clone(),
        snapshot: Some(snapshot.clone()),
    };
    let expected = persistence.saved.clone();
    let writer_path = path.clone();
    let spawned = std::thread::Builder::new()
        .name("account-save".into())
        .stack_size(256 * 1024)
        .spawn(move || {
            let result = save_current(&writer_path, &next, expected.as_ref());
            drop((writer_path, next, expected));
            diag::flush_thread_cache();
            result
        });
    let result = match spawned {
        Ok(handle) if !exiting => {
            persistence.writing = Some((snapshot, handle));
            return;
        }
        Ok(handle) => handle
            .join()
            .unwrap_or_else(|_| Err(io::Error::other("account writer panicked"))),
        Err(error) => Err(error),
    };
    finish_write(
        result,
        snapshot,
        *role,
        &path,
        &mut persistence,
        authority.as_deref_mut(),
        &mut account,
        receipt.as_deref_mut(),
    );
}

#[allow(clippy::too_many_arguments)]
fn finish_write(
    result: io::Result<()>,
    snapshot: AccountSnapshot,
    role: frame::RuntimeRole,
    path: &Path,
    persistence: &mut AccountPersistence,
    authority: Option<&mut net::AuthorityWorld>,
    account: &mut LocalAccount,
    receipt: Option<&mut net::AccountSaveReceipt>,
) {
    match result {
        Ok(()) => {
            persistence.retry_at = None;
            persistence.last_error = None;
            // A no-op once the live revision moved on during the write.
            if role == frame::RuntimeRole::Listen
                && let Some(authority) = authority
            {
                authority
                    .lock()
                    .persistent_data_mut()
                    .acknowledge_saved(account.id, snapshot.revision);
            }
            if let Some(receipt) = receipt {
                receipt.0 = Some(snapshot.clone());
            }
            persistence.saved = Some(snapshot.clone());
            account.snapshot = Some(snapshot);
        }
        Err(error) => {
            let error = (error.kind(), error.to_string());
            if persistence.last_error.as_ref() != Some(&error) {
                warn!(
                    "could not save local account {}: {}",
                    path.display(),
                    error.1
                );
                persistence.last_error = Some(error);
            }
            persistence.retry_at = Some(Instant::now() + SAVE_RETRY_DELAY);
        }
    }
}

pub(crate) fn route_unlock_commands(
    mut commands: MessageReader<crate::ConsoleCommand>,
    mut echo: crate::feature_dispatch::ConsoleEcho,
    (screen, world, role): (
        Res<frame::AppScreen>,
        Res<State<frame::MatchScope>>,
        Res<frame::RuntimeRole>,
    ),
    (account, mut authority): (
        Option<ResMut<LocalAccount>>,
        Option<ResMut<net::AuthorityWorld>>,
    ),
    mut persistence: ResMut<AccountPersistence>,
    mut receipt: Option<ResMut<net::AccountSaveReceipt>>,
    mut player_data: ResMut<sim::LocalPlayerData>,
) {
    let mut account = account;
    for command in commands.read().filter(|command| command.name == "unlock") {
        let unlock = match command.args.as_slice() {
            [mode] if mode == "all" => true,
            [mode] if mode == "reset" => false,
            _ => {
                echo.write("usage: unlock <all|reset>");
                continue;
            }
        };
        if *screen != frame::AppScreen::MainMenu
            || *world.get() == frame::MatchScope::Live
            || !matches!(
                *role,
                frame::RuntimeRole::Listen | frame::RuntimeRole::Client
            )
        {
            echo.write("unlock: disconnect to the main menu first");
            continue;
        }
        let (Some(account), Some(path)) = (account.as_mut(), persistence.path.as_ref()) else {
            echo.write("unlock: saved account is unavailable");
            continue;
        };
        if !player_data.unlock_data_ready() {
            echo.write("unlock: progression content is loading");
            continue;
        }
        let result = (|| -> Result<_, String> {
            let snapshot = player_data
                .unlock_snapshot(account.id, account.snapshot.as_ref(), unlock)
                .map_err(|error| format!("cannot edit progression: {error:?}"))?;
            let store = authority
                .as_ref()
                .filter(|authority| authority.lock().persistent_data().snapshot(account.id).is_some())
                .map(|authority| {
                    let mut store = authority.lock().persistent_data().for_new_match();
                    store.replace_unbound(snapshot.clone())?;
                    Ok::<_, sim::PersistentDataError>(store)
                })
                .transpose()
                .map_err(|error| format!("cannot replace account: {error:?}"))?;
            let next = LocalAccount {
                id: account.id,
                key: account.key.clone(),
                snapshot: Some(snapshot.clone()),
            };
            save_current(path, &next, persistence.saved.as_ref())
                .map_err(|error| format!("cannot save account: {error}"))?;
            Ok((next, snapshot, store))
        })();
        match result {
            Ok((next, snapshot, store)) => {
                if let (Some(authority), Some(store)) = (authority.as_mut(), store) {
                    authority.lock().set_persistent_data(store);
                }
                let _ = player_data.refresh(Some(&snapshot));
                persistence.saved = Some(snapshot.clone());
                persistence.retry_at = None;
                persistence.last_error = None;
                if let Some(receipt) = receipt.as_mut() {
                    receipt.0 = Some(snapshot);
                }
                **account = next;
                echo.write(if unlock {
                    "unlock all: maximum level and native challenges saved; Pro perks unlocked"
                } else {
                    "unlock reset: level 1 and initial challenge progress saved"
                });
            }
            Err(error) => echo.write(format!("unlock: {error}")),
        }
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn encode(account: &LocalAccount) -> io::Result<Vec<u8>> {
    if account.id.0 == [0; 16] {
        return Err(invalid("zero account identity"));
    }
    if account.key.account() != account.id {
        return Err(invalid("account identity does not match signing key"));
    }
    let mut bytes = Vec::with_capacity(SNAPSHOT_BYTES);
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&account.id.0);
    bytes.extend_from_slice(&account.key.export_seed());
    bytes.push(u8::from(account.snapshot.is_some()));
    if let Some(snapshot) = &account.snapshot {
        if snapshot.account != account.id || snapshot.bytes.len() != PLAYER_DATA_BUFFER_BYTES {
            return Err(invalid("account buffer identity or size mismatch"));
        }
        if snapshot.bytes[..4] != snapshot.version.to_le_bytes()
            || snapshot.bytes[4..8] != snapshot.checksum.to_le_bytes()
        {
            return Err(invalid("account buffer schema stamp mismatch"));
        }
        bytes.extend_from_slice(&snapshot.revision.to_le_bytes());
        bytes.extend_from_slice(&snapshot.version.to_le_bytes());
        bytes.extend_from_slice(&snapshot.checksum.to_le_bytes());
        bytes.extend_from_slice(&snapshot.bytes);
        bytes.extend_from_slice(&snapshot.skills.encode());
    }
    Ok(bytes)
}

fn decode(bytes: &[u8]) -> io::Result<(LocalAccount, bool)> {
    let legacy = bytes.starts_with(LEGACY_MAGIC);
    let keyed = bytes.starts_with(KEYED_MAGIC);
    let (empty, snapshot_length, tag) = if legacy {
        (LEGACY_EMPTY_BYTES, LEGACY_SNAPSHOT_BYTES, 24)
    } else if keyed {
        (EMPTY_BYTES, KEYED_SNAPSHOT_BYTES, 56)
    } else {
        (EMPTY_BYTES, SNAPSHOT_BYTES, 56)
    };
    if !matches!(bytes.len(), n if n == empty || n == snapshot_length)
        || (!legacy && !keyed && !bytes.starts_with(MAGIC))
    {
        return Err(invalid("invalid account file version or length"));
    }
    let previous_id = AccountId(bytes[8..24].try_into().unwrap());
    if previous_id.0 == [0; 16] {
        return Err(invalid("zero account identity"));
    }
    let key = if legacy {
        net::AccountKey::generate()
    } else {
        net::AccountKey::from_seed(bytes[24..56].try_into().unwrap())
    }
    .map_err(|_| invalid("invalid account signing key"))?;
    let id = key.account();
    if !legacy && previous_id != id {
        return Err(invalid("account identity does not match signing key"));
    }
    let snapshot = match bytes[tag] {
        0 if bytes.len() == empty => None,
        1 if bytes.len() == snapshot_length => {
            let header = tag + 1;
            let version = i32::from_le_bytes(bytes[header + 8..header + 12].try_into().unwrap());
            let checksum = u32::from_le_bytes(bytes[header + 12..header + 16].try_into().unwrap());
            let data_end = header + 16 + PLAYER_DATA_BUFFER_BYTES;
            let data = &bytes[header + 16..data_end];
            let skills = if legacy || keyed {
                sim::SkillRatings::default()
            } else {
                sim::SkillRatings::decode(&bytes[data_end..])
                    .map_err(|_| invalid("invalid account skill ratings"))?
            };
            if data[..4] != version.to_le_bytes() || data[4..8] != checksum.to_le_bytes() {
                return Err(invalid("account buffer schema stamp mismatch"));
            }
            Some(AccountSnapshot {
                account: id,
                revision: u64::from_le_bytes(bytes[header..header + 8].try_into().unwrap()),
                version,
                checksum,
                bytes: data.to_vec(),
                skills,
            })
        }
        _ => return Err(invalid("invalid account payload tag")),
    };
    Ok((LocalAccount { id, key, snapshot }, legacy || keyed))
}

fn read(path: &Path) -> io::Result<(LocalAccount, bool)> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take((SNAPSHOT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    decode(&bytes)
}

fn random_id() -> io::Result<[u8; 16]> {
    let mut id = [0; 16];
    while id == [0; 16] {
        getrandom::fill(&mut id).map_err(|error| io::Error::other(error.to_string()))?;
    }
    Ok(id)
}

fn lock_file(path: &Path) -> io::Result<File> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let mut name = std::ffi::OsString::from(".");
    name.push(
        path.file_name()
            .ok_or_else(|| invalid("account path has no filename"))?,
    );
    name.push(".lock");
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(parent.join(name))?;
    // Horizon has no file locks; one process owns the SD card there.
    #[cfg(not(target_os = "horizon"))]
    file.lock()?;
    Ok(file)
}

fn save_current(
    path: &Path,
    next: &LocalAccount,
    expected: Option<&AccountSnapshot>,
) -> io::Result<()> {
    let _lock = lock_file(path)?;
    let (current, legacy) = read(path)?;
    if legacy || current.id != next.id || current.key.public_key() != next.key.public_key() {
        return Err(invalid("account file owner changed since load"));
    }
    if current.snapshot == next.snapshot {
        return Ok(());
    }
    if current.snapshot.as_ref() != expected {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "account file changed since load",
        ));
    }
    write_atomic(path, next, false)
}

fn load_or_create(path: &Path) -> io::Result<LocalAccount> {
    let _lock = lock_file(path)?;
    load_or_create_locked(path)
}

fn load_or_create_locked(path: &Path) -> io::Result<LocalAccount> {
    match read(path) {
        Ok((account, migrate)) => {
            if migrate {
                write_atomic(path, &account, false)?;
            }
            return Ok(account);
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let key = net::AccountKey::generate().map_err(|_| invalid("account key generation failed"))?;
    let account = LocalAccount {
        id: key.account(),
        key,
        snapshot: None,
    };
    match write_atomic(path, &account, true) {
        Ok(()) => Ok(account),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => load_or_create_locked(path),
        Err(error) => Err(error),
    }
}

fn write_atomic(path: &Path, account: &LocalAccount, create: bool) -> io::Result<()> {
    let bytes = encode(account)?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let nonce = random_id()?;
    let suffix: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
    let temporary = parent.join(format!(".account-{suffix}.tmp"));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    let result = (|| {
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        // Horizon has no hard links, and no concurrent creator to race.
        if create && !cfg!(target_os = "horizon") {
            // Publish the first identity without replacing a concurrent creator's identity.
            fs::hard_link(&temporary, path)?;
            fs::remove_file(&temporary)?;
        } else {
            fs::rename(&temporary, path)?;
        }
        // Horizon cannot open a directory as a file.
        #[cfg(all(unix, not(target_os = "horizon")))]
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
