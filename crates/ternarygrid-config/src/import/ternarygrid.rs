//! TernaryGrid's own connections file: connections exported from one TernaryGrid (with their
//! folders, the connection types and network profiles they use) and imported into another.
//!
//! The file is JSON (`"format": "ternarygrid-connections"`). Passwords are only included when
//! the user gives a password for the file: they are then encrypted like the vault (Argon2id,
//! XChaCha20-Poly1305) and stored base64 encoded in `secrets`; without one the file holds no
//! secret at all.

use std::collections::{BTreeMap, BTreeSet};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use ternarygrid_core::config::{ConnectionConfig, ConnectionType, NetworkProfile};
use ternarygrid_core::{DbError, DbResult, Secret};

use super::ImportResult;
use crate::connections::{ConnectionStore, normalize_folder};

/// The `format` of the file.
pub const FORMAT: &str = "ternarygrid-connections";
/// The file's version.
pub const VERSION: u32 = 1;
/// The extension the export suggests.
pub const EXTENSION: &str = "json";

#[derive(Serialize, Deserialize)]
struct ExportFile {
    format: String,
    version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    app_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    exported_at: Option<String>,
    #[serde(default)]
    folders: Vec<String>,
    #[serde(default)]
    connection_types: Vec<ConnectionType>,
    #[serde(default)]
    network_profiles: Vec<NetworkProfile>,
    #[serde(default)]
    connections: Vec<ConnectionConfig>,
    /// The sealed `{secret key: value}` of the exported connections and profiles (base64).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    secrets: Option<String>,
}

/// What an export holds.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ExportSummary {
    /// Connections written.
    pub connections: usize,
    /// Navigator folders they are in (parents included).
    pub folders: usize,
    /// Connection types they use.
    pub connection_types: usize,
    /// Network profiles they use.
    pub network_profiles: usize,
    /// Passwords included (0 without a file password).
    pub secrets: usize,
}

/// The secret keys an export of `ids` may include: the connections' own and those of the
/// network profiles they use.
pub fn secret_keys(store: &ConnectionStore, ids: &[String]) -> Vec<String> {
    use ternarygrid_core::config::{SecretSlot, secret_key};
    let slots = [
        SecretSlot::Password,
        SecretSlot::SshPassword,
        SecretSlot::SshPassphrase,
        SecretSlot::ProxyPassword,
    ];
    let mut keys = Vec::new();
    let mut profiles = BTreeSet::new();
    for c in store.connections.iter().filter(|c| ids.contains(&c.id)) {
        keys.extend(slots.iter().map(|s| secret_key(&c.id, *s)));
        if let Some(p) = &c.network_profile {
            profiles.insert(p.clone());
        }
    }
    for p in profiles {
        keys.extend(
            slots
                .iter()
                .map(|s| crate::connections::profile_secret_key(&p, *s)),
        );
    }
    keys
}

/// Writes the connections `ids` of `store` (in the navigator's order) with what they need.
/// `secrets` are the values of [`secret_keys`] that exist; they are written only with a
/// `password`, encrypted.
pub fn export(
    store: &ConnectionStore,
    ids: &[String],
    secrets: &BTreeMap<String, Secret>,
    password: Option<&Secret>,
    app_version: &str,
) -> DbResult<(Vec<u8>, ExportSummary)> {
    let connections: Vec<ConnectionConfig> = store
        .connections
        .iter()
        .filter(|c| ids.contains(&c.id))
        .cloned()
        .map(|mut c| {
            // When it was last used is this machine's business.
            c.last_connected = None;
            c
        })
        .collect();
    if connections.is_empty() {
        return Err(DbError::config("Choose the connections to export"));
    }
    // Folders of the connections, with their parents.
    let mut folders = BTreeSet::new();
    for c in &connections {
        if let Some(f) = c.folder.as_deref().and_then(normalize_folder) {
            let parts: Vec<&str> = f.split('/').collect();
            for i in 1..=parts.len() {
                folders.insert(parts[..i].join("/"));
            }
        }
    }
    let type_ids: BTreeSet<&str> = connections
        .iter()
        .map(|c| c.connection_type.as_str())
        .collect();
    let connection_types: Vec<ConnectionType> = store
        .connection_types
        .iter()
        .filter(|t| type_ids.contains(t.id.as_str()))
        .cloned()
        .collect();
    let profile_ids: BTreeSet<&str> = connections
        .iter()
        .filter_map(|c| c.network_profile.as_deref())
        .collect();
    let network_profiles: Vec<NetworkProfile> = store
        .network_profiles
        .iter()
        .filter(|p| profile_ids.contains(p.id.as_str()))
        .cloned()
        .collect();
    let allowed: BTreeSet<String> = secret_keys(store, ids).into_iter().collect();
    let entries: BTreeMap<String, Secret> = secrets
        .iter()
        .filter(|(k, v)| allowed.contains(*k) && !v.is_empty())
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let (sealed, n_secrets) = match password {
        Some(pw) if !entries.is_empty() => (
            Some(
                base64::engine::general_purpose::STANDARD
                    .encode(crate::secrets::seal_with_password(&entries, pw)?),
            ),
            entries.len(),
        ),
        _ => (None, 0),
    };
    let summary = ExportSummary {
        connections: connections.len(),
        folders: folders.len(),
        connection_types: connection_types.len(),
        network_profiles: network_profiles.len(),
        secrets: n_secrets,
    };
    let file = ExportFile {
        format: FORMAT.into(),
        version: VERSION,
        app_version: Some(app_version.to_string()),
        exported_at: Some(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        folders: folders.into_iter().collect(),
        connection_types,
        network_profiles,
        connections,
        secrets: sealed,
    };
    let bytes = serde_json::to_vec_pretty(&file)
        .map_err(|e| DbError::internal(format!("Writing the connections file failed: {e}")))?;
    Ok((bytes, summary))
}

/// True for a file [`export`] wrote.
pub fn is_export_file(bytes: &[u8]) -> bool {
    #[derive(Deserialize)]
    struct Head {
        format: String,
    }
    serde_json::from_slice::<Head>(bytes).is_ok_and(|h| h.format == FORMAT)
}

/// True when the file holds encrypted passwords (opening them needs the file's password).
pub fn has_secrets(bytes: &[u8]) -> bool {
    parse(bytes).is_ok_and(|f| f.secrets.is_some())
}

fn parse(bytes: &[u8]) -> DbResult<ExportFile> {
    let file: ExportFile = serde_json::from_slice(bytes)
        .map_err(|e| DbError::config(format!("Not a TernaryGrid connections file: {e}")))?;
    if file.format != FORMAT {
        return Err(DbError::config("Not a TernaryGrid connections file"));
    }
    if file.version > VERSION {
        return Err(DbError::config(format!(
            "The file was written by a newer TernaryGrid (format {}); update TernaryGrid to import it",
            file.version
        )));
    }
    Ok(file)
}

/// Reads a file [`export`] wrote. With `password` its passwords are decrypted (a wrong one is an
/// auth error); without, they are left out with a warning.
pub fn import(bytes: &[u8], password: Option<&Secret>) -> DbResult<ImportResult> {
    let file = parse(bytes)?;
    let mut warnings = Vec::new();
    let secrets = match (&file.secrets, password) {
        (Some(sealed), Some(pw)) => {
            let raw = base64::engine::general_purpose::STANDARD
                .decode(sealed)
                .map_err(|_| DbError::config("The passwords in the file are damaged"))?;
            crate::secrets::open_with_password(&raw, pw)?
                .into_iter()
                .collect()
        }
        (Some(_), None) => {
            warnings.push(
                "The passwords were left out: the file's password was not given.".to_string(),
            );
            Vec::new()
        }
        (None, _) => Vec::new(),
    };
    Ok(ImportResult {
        connections: file.connections,
        secrets,
        folders: file.folders,
        connection_types: file.connection_types,
        network_profiles: file.network_profiles,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ternarygrid_core::DbKind;
    use ternarygrid_core::config::{SecretSlot, secret_key};

    fn store() -> ConnectionStore {
        let mut s = ConnectionStore::new();
        let mut a = ConnectionConfig::new(DbKind::Postgres, "prod");
        a.id = "a".into();
        a.folder = Some("Clients/Acme".into());
        a.host = "db.acme".into();
        let mut b = ConnectionConfig::new(DbKind::MySql, "other");
        b.id = "b".into();
        s.add(a).unwrap();
        s.add(b).unwrap();
        s
    }

    #[test]
    fn exports_the_chosen_connections_and_reads_them_back() {
        let s = store();
        let mut secrets = BTreeMap::new();
        secrets.insert(secret_key("a", SecretSlot::Password), Secret::new("pw-a"));
        secrets.insert(secret_key("b", SecretSlot::Password), Secret::new("pw-b"));
        let pw = Secret::new("file password");
        let (bytes, summary) =
            export(&s, &["a".to_string()], &secrets, Some(&pw), "0.0.0").unwrap();
        assert_eq!(
            (summary.connections, summary.folders, summary.secrets),
            (1, 2, 1)
        );
        assert!(is_export_file(&bytes) && has_secrets(&bytes));
        // The password is not readable in the file.
        assert!(!String::from_utf8_lossy(&bytes).contains("pw-a"));
        let back = import(&bytes, Some(&pw)).unwrap();
        assert_eq!(back.connections.len(), 1);
        assert_eq!(back.connections[0].host, "db.acme");
        assert_eq!(back.folders, ["Clients", "Clients/Acme"]);
        assert_eq!(
            back.secrets,
            vec![(secret_key("a", SecretSlot::Password), Secret::new("pw-a"))]
        );
        // A wrong password is refused; none leaves the passwords out with a warning.
        assert!(import(&bytes, Some(&Secret::new("wrong"))).is_err());
        let without = import(&bytes, None).unwrap();
        assert!(without.secrets.is_empty() && !without.warnings.is_empty());
    }

    #[test]
    fn without_a_password_no_secret_is_written() {
        let s = store();
        let mut secrets = BTreeMap::new();
        secrets.insert(secret_key("a", SecretSlot::Password), Secret::new("pw-a"));
        let (bytes, summary) = export(
            &s,
            &["a".to_string(), "b".to_string()],
            &secrets,
            None,
            "0.0.0",
        )
        .unwrap();
        assert_eq!((summary.connections, summary.secrets), (2, 0));
        assert!(!has_secrets(&bytes));
        assert!(!String::from_utf8_lossy(&bytes).contains("pw-a"));
        assert!(export(&s, &[], &secrets, None, "0.0.0").is_err());
        assert!(!is_export_file(b"{\"format\":\"other\"}"));
    }
}
