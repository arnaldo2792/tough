// Copyright 2019 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tough is a client library for [TUF repositories].
//!
//! This client adheres to [TUF version 1.0.0][spec], with the following exceptions:
//!
//! * Delegated roles (and TAP 3) are not yet supported.
//! * TAP 4 (multiple repository consensus) is not yet supported.
//!
//! [TUF repositories]: https://theupdateframework.github.io/
//! [spec]: https://github.com/theupdateframework/specification/blob/9f148556ca15da2ec5c022c8b3e6f99a028e5fe5/tuf-spec.md
//!
//! # Testing
//!
//! Unit tests are run in the usual manner: `cargo test`.
//! Integration tests require docker and are disabled by default behind a feature named `integ`.
//! To run all tests, including integration tests: `cargo test --all-features` or
//! `cargo test --features 'http,integ'`.

#![forbid(missing_debug_implementations, missing_copy_implementations)]
#![deny(rust_2018_idioms)]
// missing_docs is on its own line to make it easy to comment out when making changes.
#![deny(missing_docs)]
#![warn(clippy::pedantic)]
#![allow(
    clippy::module_name_repetitions,
    clippy::must_use_candidate,
    clippy::missing_errors_doc
)]

mod cache;
mod datastore;
pub mod editor;
pub mod error;
mod fetch;
#[cfg(feature = "http")]
pub mod http;
mod io;
pub mod key_source;
pub mod schema;
pub mod sign;
mod transport;

use crate::datastore::Datastore;
use crate::error::Result;
use crate::fetch::{fetch_max_size, fetch_sha256};
/// An HTTP transport that includes retries.
#[cfg(feature = "http")]
pub use crate::http::{HttpTransport, HttpTransportBuilder, RetryRead};
use crate::schema::{DelegatedRole, Delegations};
use crate::schema::{Role, RoleType, Root, Signed, Snapshot, Timestamp};
pub use crate::transport::{
    DefaultTransport, FilesystemTransport, Transport, TransportError, TransportErrorKind,
};
use chrono::{DateTime, Utc};
use snafu::{ensure, OptionExt, ResultExt};
use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use url::Url;

/// Represents whether a Repository should fail to load when metadata is expired (`Safe`) or whether
/// it should ignore expired metadata (`Unsafe`). Only use `Unsafe` if you are sure you need it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpirationEnforcement {
    /// Expirations will be enforced. You MUST use this option to get TUF security guarantees.
    Safe,

    /// Expirations will not be enforced. This is available for certain offline use cases, does NOT
    /// provide TUF security guarantees, and should only be used if you are sure that you need it.
    Unsafe,
}

/// `ExpirationEnforcement` defaults to `Safe` mode.
impl Default for ExpirationEnforcement {
    fn default() -> Self {
        ExpirationEnforcement::Safe
    }
}

impl From<bool> for ExpirationEnforcement {
    fn from(b: bool) -> Self {
        if b {
            ExpirationEnforcement::Safe
        } else {
            ExpirationEnforcement::Unsafe
        }
    }
}

impl From<ExpirationEnforcement> for bool {
    fn from(ee: ExpirationEnforcement) -> Self {
        ee == ExpirationEnforcement::Safe
    }
}

/// A builder for settings with which to load a [`Repository`]. Required settings are provided in
/// the [`RepositoryLoader::new`] function. Optional parameters can be added after calling new.
/// Finally, call [`RepositoryLoader::load`] to load the [`Repository`].
///
/// # Examples
///
/// ## Basic usage:
///
/// ```rust
/// # use std::fs::File;
/// # use std::path::PathBuf;
/// # use tough::RepositoryLoader;
/// # use url::Url;
/// # let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("data").join("tuf-reference-impl");
/// # let root = dir.join("metadata").join("1.root.json");
/// # let metadata_base_url = Url::from_file_path(dir.join("metadata")).unwrap();
/// # let targets_base_url = Url::from_file_path(dir.join("targets")).unwrap();
///
/// let repository = RepositoryLoader::new(
///     File::open(root).unwrap(),
///     metadata_base_url,
///     targets_base_url,
/// )
/// .load()
/// .unwrap();
///
/// ```
///
/// ## With optional settings:
///
/// ```rust
/// # use std::fs::File;
/// # use std::path::PathBuf;
/// # use tough::{RepositoryLoader, FilesystemTransport, ExpirationEnforcement};
/// # use url::Url;
/// # let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("data").join("tuf-reference-impl");
/// # let root = dir.join("metadata").join("1.root.json");
/// # let metadata_base_url = Url::from_file_path(dir.join("metadata")).unwrap();
/// # let targets_base_url = Url::from_file_path(dir.join("targets")).unwrap();
///
/// let repository = RepositoryLoader::new(
///     File::open(root).unwrap(),
///     metadata_base_url,
///     targets_base_url,
/// )
/// .transport(FilesystemTransport)
/// .expiration_enforcement(ExpirationEnforcement::Unsafe)
/// .load()
/// .unwrap();
///
/// ```
#[derive(Debug, Clone)]
pub struct RepositoryLoader<R>
where
    R: Read,
{
    root: R,
    metadata_base_url: Url,
    targets_base_url: Url,
    transport: Option<Box<dyn Transport>>,
    limits: Option<Limits>,
    datastore: Option<PathBuf>,
    expiration_enforcement: Option<ExpirationEnforcement>,
}

impl<R: Read> RepositoryLoader<R> {
    /// Create a new `RepositoryLoader`.
    ///
    /// `root` is a [`Read`]er for the trusted root metadata file, which you must ship with your
    /// software using an out-of-band process. It should be a copy of the most recent root.json
    /// from your repository. (It's okay if it becomes out of date later; the client establishes
    /// trust up to the most recent root.json file.)
    ///
    /// `metadata_base_url` and `targets_base_url` are the base URLs where the client can find
    /// metadata (such as root.json) and targets (as listed in targets.json).
    pub fn new(root: R, metadata_base_url: Url, targets_base_url: Url) -> Self {
        Self {
            root,
            metadata_base_url,
            targets_base_url,
            transport: None,
            limits: None,
            datastore: None,
            expiration_enforcement: None,
        }
    }

    /// Load and verify TUF repository metadata.
    pub fn load(self) -> Result<Repository> {
        Repository::load(self)
    }

    /// Set the transport. If no transport has been set, [`DefaultTransport`] will be used.
    pub fn transport<T: Transport + 'static>(mut self, transport: T) -> Self {
        self.transport = Some(Box::new(transport));
        self
    }

    /// Set a the repository [`Limits`].
    pub fn limits(mut self, limits: Limits) -> Self {
        self.limits = Some(limits);
        self
    }

    /// Set a `datastore` directory path. `datastore` is a directory on a persistent filesystem.
    /// This directory's contents store the most recently fetched timestamp, snapshot, and targets
    /// metadata files to detect version rollback attacks.
    ///
    /// You may chose to provide a [`PathBuf`] to a directory on a persistent filesystem, which must
    /// exist prior to calling [`RepositoryLoader::load`]. If no datastore is provided, a temporary
    /// directory will be created and cleaned up for for you.
    pub fn datastore<P: Into<PathBuf>>(mut self, datastore: P) -> Self {
        self.datastore = Some(datastore.into());
        self
    }

    /// Set the [`ExpirationEnforcement`].
    ///
    /// **CAUTION:** TUF metadata expiration dates, particularly `timestamp.json`, are designed to
    /// limit a replay attack window. By setting `expiration_enforcement` to `Unsafe`, you are
    /// disabling this feature of TUF. Use `Safe` unless you have a good reason to use `Unsafe`.
    pub fn expiration_enforcement(mut self, exp: ExpirationEnforcement) -> Self {
        self.expiration_enforcement = Some(exp);
        self
    }
}

/// Limits used when fetching repository metadata.
///
/// These limits are implemented to prevent endless data attacks. Clients must ensure these values
/// are set higher than what would reasonably be expected by a repository, but not so high that the
/// amount of data could interfere with the system.
///
/// `max_root_size` and `max_timestamp_size` are the maximum size for the `root.json` and
/// `timestamp.json` files, respectively, downloaded from the repository. These must be
/// sufficiently large such that future updates to your repository's key management strategy
/// will still be supported, but sufficiently small such that you are protected against an
/// endless data attack (defined by TUF as an attacker responding to clients with extremely
/// large files that interfere with the client's system).
///
/// The [`Default`] implementation sets the following values:
/// * `max_root_size`: 1 MiB
/// * `max_targets_size`: 10 MiB
/// * `max_timestamp_size`: 1 MiB
/// * `max_root_updates`: 1024
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// The maximum allowable size in bytes for downloaded root.json files.
    pub max_root_size: u64,

    /// The maximum allowable size in bytes for downloaded targets.json file **if** the size is not
    /// listed in snapshots.json. This setting is ignored if the size of targets.json is in the
    /// signed snapshots.json file.
    pub max_targets_size: u64,

    /// The maximum allowable size in bytes for the downloaded timestamp.json file.
    pub max_timestamp_size: u64,

    /// The maximum number of updates to root.json to download.
    pub max_root_updates: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_root_size: 1024 * 1024,         // 1 MiB
            max_targets_size: 1024 * 1024 * 10, // 10 MiB
            max_timestamp_size: 1024 * 1024,    // 1 MiB
            max_root_updates: 1024,
        }
    }
}

/// A TUF repository.
///
/// You can create a `Repository` using a [`RepositoryLoader`].
#[derive(Debug, Clone)]
pub struct Repository {
    transport: Box<dyn Transport>,
    consistent_snapshot: bool,
    datastore: Datastore,
    earliest_expiration: DateTime<Utc>,
    earliest_expiration_role: RoleType,
    root: Signed<Root>,
    snapshot: Signed<Snapshot>,
    timestamp: Signed<Timestamp>,
    targets: Signed<crate::schema::Targets>,
    limits: Limits,
    metadata_base_url: Url,
    targets_base_url: Url,
    expiration_enforcement: ExpirationEnforcement,
}

impl Repository {
    /// Load and verify TUF repository metadata using a [`RepositoryLoader`] for the settings.
    fn load<R: Read>(loader: RepositoryLoader<R>) -> Result<Self> {
        let datastore = Datastore::new(loader.datastore)?;
        let transport = loader
            .transport
            .unwrap_or_else(|| Box::new(DefaultTransport::new()));
        let limits = loader.limits.unwrap_or_default();
        let expiration_enforcement = loader.expiration_enforcement.unwrap_or_default();
        let metadata_base_url = parse_url(loader.metadata_base_url)?;
        let targets_base_url = parse_url(loader.targets_base_url)?;

        // 0. Load the trusted root metadata file + 1. Update the root metadata file
        let root = load_root(
            transport.as_ref(),
            loader.root,
            &datastore,
            limits.max_root_size,
            limits.max_root_updates,
            &metadata_base_url,
            expiration_enforcement,
        )?;

        // 2. Download the timestamp metadata file
        let timestamp = load_timestamp(
            transport.as_ref(),
            &root,
            &datastore,
            limits.max_timestamp_size,
            &metadata_base_url,
            expiration_enforcement,
        )?;

        // 3. Download the snapshot metadata file
        let snapshot = load_snapshot(
            transport.as_ref(),
            &root,
            &timestamp,
            &datastore,
            &metadata_base_url,
            expiration_enforcement,
        )?;

        // 4. Download the targets metadata file
        let targets = load_targets(
            transport.as_ref(),
            &root,
            &snapshot,
            &datastore,
            limits.max_targets_size,
            &metadata_base_url,
            expiration_enforcement,
        )?;

        let expires_iter = [
            (root.signed.expires, RoleType::Root),
            (timestamp.signed.expires, RoleType::Timestamp),
            (snapshot.signed.expires, RoleType::Snapshot),
            (targets.signed.expires, RoleType::Targets),
        ];
        let (earliest_expiration, earliest_expiration_role) =
            expires_iter.iter().min_by_key(|tup| tup.0).unwrap();

        Ok(Self {
            transport,
            consistent_snapshot: root.signed.consistent_snapshot,
            datastore,
            earliest_expiration: *earliest_expiration,
            earliest_expiration_role: *earliest_expiration_role,
            root,
            snapshot,
            timestamp,
            targets,
            limits,
            metadata_base_url,
            targets_base_url,
            expiration_enforcement,
        })
    }

    /// Returns the list of targets present in the repository.
    pub fn targets(&self) -> &Signed<crate::schema::Targets> {
        &self.targets
    }

    /// Returns a reference to the signed root
    pub fn root(&self) -> &Signed<Root> {
        &self.root
    }

    /// Returns a reference to the signed snapshot
    pub fn snapshot(&self) -> &Signed<Snapshot> {
        &self.snapshot
    }

    /// Returns a reference to the signed timestamp
    pub fn timestamp(&self) -> &Signed<Timestamp> {
        &self.timestamp
    }

    ///return a vec of all targets including all target files delegated by targets
    pub fn all_targets(&self) -> impl Iterator + '_ {
        self.targets.signed.targets_iter()
    }

    /// Fetches a target from the repository.
    ///
    /// If the repository metadata is expired or there is an issue making the request, `Err` is
    /// returned.
    ///
    /// If the requested target is not listed in the repository metadata, `Ok(None)` is returned.
    ///
    /// Otherwise, a reader is returned, which provides streaming access to the target contents
    /// before its checksum is validated. If the maximum size is reached or there is a checksum
    /// mismatch, the reader returns a [`std::io::Error`]. **Consumers of this library must not use
    /// data from the reader if it returns an error.**
    pub fn read_target(&self, name: &str) -> Result<Option<impl Read + Send>> {
        // Check for repository metadata expiration.
        if self.expiration_enforcement == ExpirationEnforcement::Safe {
            ensure!(
                system_time(&self.datastore)? < self.earliest_expiration,
                error::ExpiredMetadata {
                    role: self.earliest_expiration_role
                }
            );
        }

        // 5. Verify the desired target against its targets metadata.
        //
        // 5.1. If there is no targets metadata about this target, abort the update cycle and
        //   report that there is no such target.
        //
        // 5.2. Otherwise, download the target (up to the number of bytes specified in the targets
        //   metadata), and verify that its hashes match the targets metadata. (We download up to
        //   this number of bytes, because in some cases, the exact number is unknown. This may
        //   happen, for example, if an external program is used to compute the root hash of a tree
        //   of targets files, and this program does not provide the total size of all of these
        //   files.) If consistent snapshots are not used (see Section 7), then the filename used
        //   to download the target file is of the fixed form FILENAME.EXT (e.g., foobar.tar.gz).
        //   Otherwise, the filename is of the form HASH.FILENAME.EXT (e.g.,
        //   c14aeb4ac9f4a8fc0d83d12482b9197452f6adf3eb710e3b1e2b79e8d14cb681.foobar.tar.gz), where
        //   HASH is one of the hashes of the targets file listed in the targets metadata file
        //   found earlier in step 4. In either case, the client MUST write the file to
        //   non-volatile storage as FILENAME.EXT.
        Ok(if let Ok(target) = self.targets.signed.find_target(name) {
            let (sha256, file) = self.target_digest_and_filename(target, name);
            Some(self.fetch_target(target, &sha256, file.as_str())?)
        } else {
            None
        })
    }

    /// Return the named `DelegatedRole` if found.
    pub fn delegated_role(&self, name: &str) -> Option<&DelegatedRole> {
        self.targets.signed.delegated_role(name).ok()
    }
}

/// Ensures that system time has not stepped backward since it was last sampled
fn system_time(datastore: &Datastore) -> Result<DateTime<Utc>> {
    let file = "latest_known_time.json";
    // Get 'current' system time
    let sys_time = Utc::now();
    // Load the latest known system time, if it exists
    if let Some(Ok(latest_known_time)) = datastore
        .reader(file)?
        .map(serde_json::from_reader::<_, DateTime<Utc>>)
    {
        // Make sure the sampled system time did not go back in time
        ensure!(
            sys_time >= latest_known_time,
            error::SystemTimeSteppedBackward {
                sys_time,
                latest_known_time
            }
        );
    }
    // Store the latest known time
    // Serializes RFC3339 time string and store to datastore
    datastore.create(file, &sys_time)?;
    Ok(sys_time)
}

/// TUF v1.0.16, 5.2.9, 5.3.3, 5.4.5, 5.5.4, The expiration timestamp in the `[metadata]` file MUST
/// be higher than the fixed update start time.
fn check_expired<T: Role>(datastore: &Datastore, role: &T) -> Result<()> {
    ensure!(
        system_time(datastore)? <= role.expires(),
        error::ExpiredMetadata { role: T::TYPE }
    );
    Ok(())
}

/// Checks to see if the `Url` has a trailing slash and adds one if not. Without a trailing slash,
/// the last component of a `Url` is considered to be a file. `metadata_url` and `targets_url`
/// must refer to a base (i.e. directory), so we need them to end with a slash.
fn parse_url(url: Url) -> Result<Url> {
    if url.as_str().ends_with('/') {
        Ok(url)
    } else {
        let mut s = url.to_string();
        s.push('/');
        Url::parse(&s).context(error::ParseUrl { url: s })
    }
}

/// Steps 0 and 1 of the client application, which load the current root metadata file based on a
/// trusted root metadata file.
fn load_root<R: Read>(
    transport: &dyn Transport,
    root: R,
    datastore: &Datastore,
    max_root_size: u64,
    max_root_updates: u64,
    metadata_base_url: &Url,
    expiration_enforcement: ExpirationEnforcement,
) -> Result<Signed<Root>> {
    // 0. Load the trusted root metadata file. We assume that a good, trusted copy of this file was
    //    shipped with the package manager or software updater using an out-of-band process. Note
    //    that the expiration of the trusted root metadata file does not matter, because we will
    //    attempt to update it in the next step.
    let mut root: Signed<Root> =
        serde_json::from_reader(root).context(error::ParseTrustedMetadata)?;
    root.signed
        .verify_role(&root)
        .context(error::VerifyTrustedMetadata)?;

    // Used in step 1.2
    let original_root_version = root.signed.version.get();

    // Used in step 1.9
    let original_timestamp_keys = root
        .signed
        .keys(RoleType::Timestamp)
        .cloned()
        .collect::<Vec<_>>();
    let original_snapshot_keys = root
        .signed
        .keys(RoleType::Snapshot)
        .cloned()
        .collect::<Vec<_>>();

    // 1. Update the root metadata file. Since it may now be signed using entirely different keys,
    //    the client must somehow be able to establish a trusted line of continuity to the latest
    //    set of keys. To do so, the client MUST download intermediate root metadata files, until
    //    the latest available one is reached. Therefore, it MUST temporarily turn on consistent
    //    snapshots in order to download versioned root metadata files as described next.
    loop {
        // 1.1. Let N denote the version number of the trusted root metadata file.
        //
        // 1.2. Try downloading version N+1 of the root metadata file, up to some X number of bytes
        //   (because the size is unknown). The value for X is set by the authors of the
        //   application using TUF. For example, X may be tens of kilobytes. The filename used to
        //   download the root metadata file is of the fixed form VERSION_NUMBER.FILENAME.EXT
        //   (e.g., 42.root.json). If this file is not available, or we have downloaded more than Y
        //   number of root metadata files (because the exact number is as yet unknown), then go to
        //   step 1.8. The value for Y is set by the authors of the application using TUF. For
        //   example, Y may be 2^10.
        ensure!(
            root.signed.version.get() < original_root_version + max_root_updates,
            error::MaxUpdatesExceeded { max_root_updates }
        );
        let path = format!("{}.root.json", root.signed.version.get() + 1);
        match fetch_max_size(
            transport,
            metadata_base_url.join(&path).context(error::JoinUrl {
                path,
                url: metadata_base_url.clone(),
            })?,
            max_root_size,
            "max_root_size argument",
        ) {
            Err(_) => break, // If this file is not available, then go to step 1.8.
            Ok(reader) => {
                let new_root: Signed<Root> =
                    serde_json::from_reader(reader).context(error::ParseMetadata {
                        role: RoleType::Root,
                    })?;

                // 1.3. Check signatures. Version N+1 of the root metadata file MUST have been
                //   signed by: (1) a threshold of keys specified in the trusted root metadata file
                //   (version N), and (2) a threshold of keys specified in the new root metadata
                //   file being validated (version N+1). If version N+1 is not signed as required,
                //   discard it, abort the update cycle, and report the signature failure. On the
                //   next update cycle, begin at step 0 and version N of the root metadata file.
                root.signed
                    .verify_role(&new_root)
                    .context(error::VerifyMetadata {
                        role: RoleType::Root,
                    })?;
                new_root
                    .signed
                    .verify_role(&new_root)
                    .context(error::VerifyMetadata {
                        role: RoleType::Root,
                    })?;

                // 1.4. Check for a rollback attack. The version number of the trusted root
                //   metadata file (version N) must be less than or equal to the version number of
                //   the new root metadata file (version N+1). Effectively, this means checking
                //   that the version number signed in the new root metadata file is indeed N+1. If
                //   the version of the new root metadata file is less than the trusted metadata
                //   file, discard it, abort the update cycle, and report the rollback attack. On
                //   the next update cycle, begin at step 0 and version N of the root metadata
                //   file.
                ensure!(
                    root.signed.version <= new_root.signed.version,
                    error::OlderMetadata {
                        role: RoleType::Root,
                        current_version: root.signed.version,
                        new_version: new_root.signed.version
                    }
                );

                // Off-spec: 1.4 specifies that the version number of the trusted root metadata
                // file must be less than or equal to the version number of the new root metadata
                // file. If they are equal, this will create an infinite loop, so we ignore the new
                // root metadata file but do not report an error. This could only happen if the
                // path we built above, referencing N+1, has a filename that doesn't match its
                // contents, which would have to list version N.
                if root.signed.version == new_root.signed.version {
                    break;
                }

                // 1.5. Note that the expiration of the new (intermediate) root metadata file does
                //   not matter yet, because we will check for it in step 1.8.
                //
                // 1.6. Set the trusted root metadata file to the new root metadata file.
                //
                // (This is where version N+1 becomes version N.)
                root = new_root;

                // 1.7. Repeat steps 1.1 to 1.7.
                continue;
            }
        }
    }

    // TUF v1.0.16, 5.2.9. Check for a freeze attack. The expiration timestamp in the trusted root
    // metadata file MUST be higher than the fixed update start time. If the trusted root metadata
    // file has expired, abort the update cycle, report the potential freeze attack. On the next
    // update cycle, begin at step 5.1 and version N of the root metadata file.
    if expiration_enforcement == ExpirationEnforcement::Safe {
        check_expired(datastore, &root.signed)?;
    }

    // 1.9. If the timestamp and / or snapshot keys have been rotated, then delete the trusted
    //   timestamp and snapshot metadata files. This is done in order to recover from fast-forward
    //   attacks after the repository has been compromised and recovered. A fast-forward attack
    //   happens when attackers arbitrarily increase the version numbers of: (1) the timestamp
    //   metadata, (2) the snapshot metadata, and / or (3) the targets, or a delegated targets,
    //   metadata file in the snapshot metadata.
    if original_timestamp_keys
        .iter()
        .ne(root.signed.keys(RoleType::Timestamp))
        || original_snapshot_keys
            .iter()
            .ne(root.signed.keys(RoleType::Snapshot))
    {
        let r1 = datastore.remove("timestamp.json");
        let r2 = datastore.remove("snapshot.json");
        r1.and(r2)?;
    }

    // 1.10. Set whether consistent snapshots are used as per the trusted root metadata file (see
    //   Section 4.3).
    //
    // (This is done by checking the value of root.signed.consistent_snapshot throughout this
    // library.)

    Ok(root)
}

/// Step 2 of the client application, which loads the timestamp metadata file.
fn load_timestamp(
    transport: &dyn Transport,
    root: &Signed<Root>,
    datastore: &Datastore,
    max_timestamp_size: u64,
    metadata_base_url: &Url,
    expiration_enforcement: ExpirationEnforcement,
) -> Result<Signed<Timestamp>> {
    // 2. Download the timestamp metadata file, up to Y number of bytes (because the size is
    //    unknown.) The value for Y is set by the authors of the application using TUF. For
    //    example, Y may be tens of kilobytes. The filename used to download the timestamp metadata
    //    file is of the fixed form FILENAME.EXT (e.g., timestamp.json).
    let path = "timestamp.json";
    let reader = fetch_max_size(
        transport,
        metadata_base_url.join(path).context(error::JoinUrl {
            path,
            url: metadata_base_url.clone(),
        })?,
        max_timestamp_size,
        "max_timestamp_size argument",
    )?;
    let timestamp: Signed<Timestamp> =
        serde_json::from_reader(reader).context(error::ParseMetadata {
            role: RoleType::Timestamp,
        })?;

    // 2.1. Check signatures. The new timestamp metadata file must have been signed by a threshold
    //   of keys specified in the trusted root metadata file. If the new timestamp metadata file is
    //   not properly signed, discard it, abort the update cycle, and report the signature failure.
    root.signed
        .verify_role(&timestamp)
        .context(error::VerifyMetadata {
            role: RoleType::Timestamp,
        })?;

    // 2.2. Check for a rollback attack. The version number of the trusted timestamp metadata file,
    //   if any, must be less than or equal to the version number of the new timestamp metadata
    //   file. If the new timestamp metadata file is older than the trusted timestamp metadata
    //   file, discard it, abort the update cycle, and report the potential rollback attack.
    if let Some(Ok(old_timestamp)) = datastore
        .reader("timestamp.json")?
        .map(serde_json::from_reader::<_, Signed<Timestamp>>)
    {
        if root.signed.verify_role(&old_timestamp).is_ok() {
            ensure!(
                old_timestamp.signed.version <= timestamp.signed.version,
                error::OlderMetadata {
                    role: RoleType::Timestamp,
                    current_version: old_timestamp.signed.version,
                    new_version: timestamp.signed.version
                }
            );
        }
    }

    // TUF v1.0.16, 5.3.3. Check for a freeze attack. The expiration timestamp in the new timestamp
    // metadata file MUST be higher than the fixed update start time. If so, the new timestamp
    // metadata file becomes the trusted timestamp metadata file. If the new timestamp metadata file
    // has expired, discard it, abort the update cycle, and report the potential freeze attack.
    if expiration_enforcement == ExpirationEnforcement::Safe {
        check_expired(datastore, &timestamp.signed)?;
    }

    // Now that everything seems okay, write the timestamp file to the datastore.
    datastore.create("timestamp.json", &timestamp)?;

    Ok(timestamp)
}

/// Step 3 of the client application, which loads the snapshot metadata file.
fn load_snapshot(
    transport: &dyn Transport,
    root: &Signed<Root>,
    timestamp: &Signed<Timestamp>,
    datastore: &Datastore,
    metadata_base_url: &Url,
    expiration_enforcement: ExpirationEnforcement,
) -> Result<Signed<Snapshot>> {
    // 3. Download snapshot metadata file, up to the number of bytes specified in the timestamp
    //    metadata file. If consistent snapshots are not used (see Section 7), then the filename
    //    used to download the snapshot metadata file is of the fixed form FILENAME.EXT (e.g.,
    //    snapshot.json). Otherwise, the filename is of the form VERSION_NUMBER.FILENAME.EXT (e.g.,
    //    42.snapshot.json), where VERSION_NUMBER is the version number of the snapshot metadata
    //    file listed in the timestamp metadata file. In either case, the client MUST write the
    //    file to non-volatile storage as FILENAME.EXT.
    let snapshot_meta = timestamp
        .signed
        .meta
        .get("snapshot.json")
        .context(error::MetaMissing {
            file: "snapshot.json",
            role: RoleType::Timestamp,
        })?;
    let path = if root.signed.consistent_snapshot {
        format!("{}.snapshot.json", snapshot_meta.version)
    } else {
        "snapshot.json".to_owned()
    };
    let reader = fetch_sha256(
        transport,
        metadata_base_url.join(&path).context(error::JoinUrl {
            path,
            url: metadata_base_url.clone(),
        })?,
        snapshot_meta.length,
        "timestamp.json",
        &snapshot_meta.hashes.sha256,
    )?;
    let snapshot: Signed<Snapshot> =
        serde_json::from_reader(reader).context(error::ParseMetadata {
            role: RoleType::Snapshot,
        })?;

    // 3.1. Check against timestamp metadata. The hashes and version number of the new snapshot
    //   metadata file MUST match the hashes and version number listed in timestamp metadata. If
    //   hashes and version do not match, discard the new snapshot metadata, abort the update
    //   cycle, and report the failure.
    //
    // (We already checked the hash in `fetch_sha256` above.)
    ensure!(
        snapshot.signed.version == snapshot_meta.version,
        error::VersionMismatch {
            role: RoleType::Snapshot,
            fetched: snapshot.signed.version,
            expected: snapshot_meta.version
        }
    );

    // 3.2. Check signatures. The new snapshot metadata file MUST have been signed by a threshold
    //   of keys specified in the trusted root metadata file. If the new snapshot metadata file is
    //   not signed as required, discard it, abort the update cycle, and report the signature
    //   failure.
    root.signed
        .verify_role(&snapshot)
        .context(error::VerifyMetadata {
            role: RoleType::Snapshot,
        })?;

    // 3.3. Check for a rollback attack.
    //
    // 3.3.1. Note that the trusted snapshot metadata file may be checked for authenticity, but its
    //   expiration does not matter for the following purposes.
    if let Some(Ok(old_snapshot)) = datastore
        .reader("snapshot.json")?
        .map(serde_json::from_reader::<_, Signed<Snapshot>>)
    {
        // 3.3.2. The version number of the trusted snapshot metadata file, if any, MUST be less
        //   than or equal to the version number of the new snapshot metadata file. If the new
        //   snapshot metadata file is older than the trusted metadata file, discard it, abort the
        //   update cycle, and report the potential rollback attack.
        if root.signed.verify_role(&old_snapshot).is_ok() {
            ensure!(
                old_snapshot.signed.version <= snapshot.signed.version,
                error::OlderMetadata {
                    role: RoleType::Snapshot,
                    current_version: old_snapshot.signed.version,
                    new_version: snapshot.signed.version
                }
            );

            // 3.3.3. The version number of the targets metadata file, and all delegated targets
            //   metadata files (if any), in the trusted snapshot metadata file, if any, MUST be
            //   less than or equal to its version number in the new snapshot metadata file.
            //   Furthermore, any targets metadata filename that was listed in the trusted snapshot
            //   metadata file, if any, MUST continue to be listed in the new snapshot metadata
            //   file. If any of these conditions are not met, discard the new snaphot metadadata
            //   file, abort the update cycle, and report the failure.
            if let Some(old_targets_meta) = old_snapshot.signed.meta.get("targets.json") {
                let targets_meta =
                    snapshot
                        .signed
                        .meta
                        .get("targets.json")
                        .context(error::MetaMissing {
                            file: "targets.json",
                            role: RoleType::Snapshot,
                        })?;
                ensure!(
                    old_targets_meta.version <= targets_meta.version,
                    error::OlderMetadata {
                        role: RoleType::Targets,
                        current_version: old_targets_meta.version,
                        new_version: targets_meta.version,
                    }
                );
            }
        }
    }

    // TUF v1.0.16, 5.4.5. Check for a freeze attack. The expiration timestamp in the new snapshot
    // metadata file MUST be higher than the fixed update start time. If so, the new snapshot
    // metadata file becomes the trusted snapshot metadata file. If the new snapshot metadata file
    // is expired, discard it, abort the update cycle, and report the potential freeze attack.
    if expiration_enforcement == ExpirationEnforcement::Safe {
        check_expired(datastore, &snapshot.signed)?;
    }

    // Now that everything seems okay, write the snapshot file to the datastore.
    datastore.create("snapshot.json", &snapshot)?;

    Ok(snapshot)
}

/// Step 4 of the client application, which loads the targets metadata file.
fn load_targets(
    transport: &dyn Transport,
    root: &Signed<Root>,
    snapshot: &Signed<Snapshot>,
    datastore: &Datastore,
    max_targets_size: u64,
    metadata_base_url: &Url,
    expiration_enforcement: ExpirationEnforcement,
) -> Result<Signed<crate::schema::Targets>> {
    // 4. Download the top-level targets metadata file, up to either the number of bytes specified
    //    in the snapshot metadata file, or some Z number of bytes. The value for Z is set by the
    //    authors of the application using TUF. For example, Z may be tens of kilobytes. If
    //    consistent snapshots are not used (see Section 7), then the filename used to download the
    //    targets metadata file is of the fixed form FILENAME.EXT (e.g., targets.json).  Otherwise,
    //    the filename is of the form VERSION_NUMBER.FILENAME.EXT (e.g., 42.targets.json), where
    //    VERSION_NUMBER is the version number of the targets metadata file listed in the snapshot
    //    metadata file. In either case, the client MUST write the file to non-volatile storage as
    //    FILENAME.EXT.
    let targets_meta = snapshot
        .signed
        .meta
        .get("targets.json")
        .context(error::MetaMissing {
            file: "targets.json",
            role: RoleType::Timestamp,
        })?;
    let path = if root.signed.consistent_snapshot {
        format!("{}.targets.json", targets_meta.version)
    } else {
        "targets.json".to_owned()
    };
    let targets_url = metadata_base_url.join(&path).context(error::JoinUrl {
        path,
        url: metadata_base_url.clone(),
    })?;
    let (max_targets_size, specifier) = match targets_meta.length {
        Some(length) => (length, "snapshot.json"),
        None => (max_targets_size, "max_targets_size parameter"),
    };
    let reader = if let Some(hashes) = &targets_meta.hashes {
        Box::new(fetch_sha256(
            transport,
            targets_url,
            max_targets_size,
            specifier,
            &hashes.sha256,
        )?) as Box<dyn Read>
    } else {
        Box::new(fetch_max_size(
            transport,
            targets_url,
            max_targets_size,
            specifier,
        )?)
    };
    let mut targets: Signed<crate::schema::Targets> =
        serde_json::from_reader(reader).context(error::ParseMetadata {
            role: RoleType::Targets,
        })?;

    // 4.1. Check against snapshot metadata. The hashes (if any), and version number of the new
    //   targets metadata file MUST match the trusted snapshot metadata. This is done, in part, to
    //   prevent a mix-and-match attack by man-in-the-middle attackers. If the new targets metadata
    //   file does not match, discard it, abort the update cycle, and report the failure.
    //
    // (We already checked the hash in `fetch_sha256` above.)
    ensure!(
        targets.signed.version == targets_meta.version,
        error::VersionMismatch {
            role: RoleType::Targets,
            fetched: targets.signed.version,
            expected: targets_meta.version
        }
    );

    // 4.2. Check for an arbitrary software attack. The new targets metadata file MUST have been
    //   signed by a threshold of keys specified in the trusted root metadata file. If the new
    //   targets metadata file is not signed as required, discard it, abort the update cycle, and
    //   report the failure.
    root.signed
        .verify_role(&targets)
        .context(error::VerifyMetadata {
            role: RoleType::Targets,
        })?;

    // 4.3. Check for a rollback attack. The version number of the trusted targets metadata file,
    //   if any, MUST be less than or equal to the version number of the new targets metadata file.
    //   If the new targets metadata file is older than the trusted targets metadata file, discard
    //   it, abort the update cycle, and report the potential rollback attack.
    if let Some(Ok(old_targets)) = datastore
        .reader("targets.json")?
        .map(serde_json::from_reader::<_, Signed<crate::schema::Targets>>)
    {
        if root.signed.verify_role(&old_targets).is_ok() {
            ensure!(
                old_targets.signed.version <= targets.signed.version,
                error::OlderMetadata {
                    role: RoleType::Targets,
                    current_version: old_targets.signed.version,
                    new_version: targets.signed.version
                }
            );
        }
    }

    // TUF v1.0.16, 5.5.4. Check for a freeze attack. The expiration timestamp in the new targets
    // metadata file MUST be higher than the fixed update start time. If so, the new targets
    // metadata file becomes the trusted targets metadata file. If the new targets metadata file is
    // expired, discard it, abort the update cycle, and report the potential freeze attack.
    if expiration_enforcement == ExpirationEnforcement::Safe {
        check_expired(datastore, &targets.signed)?;
    }

    // Now that everything seems okay, write the targets file to the datastore.
    datastore.create("targets.json", &targets)?;

    // 4.5. Perform a preorder depth-first search for metadata about the desired target, beginning
    //   with the top-level targets role.
    if let Some(delegations) = &mut targets.signed.delegations {
        load_delegations(
            transport,
            snapshot,
            root.signed.consistent_snapshot,
            metadata_base_url,
            max_targets_size,
            delegations,
            datastore,
        )?;
    }

    Ok(targets)
}

// Follow the paths of delegations starting with the top level targets.json delegation
fn load_delegations(
    transport: &dyn Transport,
    snapshot: &Signed<Snapshot>,
    consistent_snapshot: bool,
    metadata_base_url: &Url,
    max_targets_size: u64,
    delegation: &mut Delegations,
    datastore: &Datastore,
) -> Result<()> {
    let mut delegated_roles: HashMap<String, Option<Signed<crate::schema::Targets>>> =
        HashMap::new();
    for delegated_role in &delegation.roles {
        // find the role file metadata
        let role_meta = snapshot
            .signed
            .meta
            .get(&format!("{}.json", &delegated_role.name))
            .context(error::RoleNotInMeta {
                name: delegated_role.name.clone(),
            })?;

        let path = if consistent_snapshot {
            format!("{}.{}.json", &role_meta.version, &delegated_role.name)
        } else {
            format!("{}.json", &delegated_role.name)
        };
        let role_url = metadata_base_url.join(&path).context(error::JoinUrl {
            path: path.clone(),
            url: metadata_base_url.clone(),
        })?;
        let specifier = "max_targets_size parameter";
        // load the role json file
        let reader = Box::new(fetch_max_size(
            transport,
            role_url,
            max_targets_size,
            specifier,
        )?);
        // since each role is a targets, we load them as such
        let role: Signed<crate::schema::Targets> =
            serde_json::from_reader(reader).context(error::ParseMetadata {
                role: RoleType::Targets,
            })?;
        // verify each role with the delegation
        delegation
            .verify_role(&role, &delegated_role.name)
            .context(error::VerifyMetadata {
                role: RoleType::Targets,
            })?;
        ensure!(
            role.signed.version == role_meta.version,
            error::VersionMismatch {
                role: RoleType::Targets,
                fetched: role.signed.version,
                expected: role_meta.version
            }
        );
        {
            if let Some(delegations) = role.signed.delegations.as_ref() {
                delegations.verify_paths().context(error::InvalidPath {})?;
            }
        }

        datastore.create(&path, &role)?;
        delegated_roles.insert(delegated_role.name.clone(), Some(role));
    }
    // load all roles delegated by this role
    for delegated_role in &mut delegation.roles {
        delegated_role.targets = delegated_roles.remove(&delegated_role.name).context(
            error::DelegatedRolesNotConsistent {
                name: delegated_role.name.clone(),
            },
        )?;
        if let Some(targets) = &mut delegated_role.targets {
            if let Some(delegations) = &mut targets.signed.delegations {
                load_delegations(
                    transport,
                    snapshot,
                    consistent_snapshot,
                    metadata_base_url,
                    max_targets_size,
                    delegations,
                    datastore,
                )?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Check if a url with a trailing slash and one without trailing slash can both be parsed
    #[test]
    fn url_missing_trailing_slash() {
        let parsed_url_without_trailing_slash =
            parse_url(Url::parse("https://example.org/a/b/c").unwrap()).unwrap();
        let parsed_url_with_trailing_slash =
            parse_url(Url::parse("https://example.org/a/b/c/").unwrap()).unwrap();
        assert_eq!(
            parsed_url_without_trailing_slash,
            parsed_url_with_trailing_slash
        );
    }

    // Ensure that the `ExpirationEnforcement` traits are not changed by mistake.
    #[test]
    fn expiration_enforcement_traits() {
        let enforce = true;
        let safe: ExpirationEnforcement = enforce.into();
        assert_eq!(safe, ExpirationEnforcement::Safe);
        let not_enforce = false;
        let not_safe: ExpirationEnforcement = not_enforce.into();
        assert_eq!(not_safe, ExpirationEnforcement::Unsafe);
        let enforcing: bool = ExpirationEnforcement::Safe.into();
        assert!(enforcing);
        let non_enforcing: bool = ExpirationEnforcement::Unsafe.into();
        assert!(!non_enforcing);
        let default = ExpirationEnforcement::default();
        assert_eq!(default, ExpirationEnforcement::Safe);
    }
}
