// Copyright 2019 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::download_root::download_root;
use crate::error::{self, Result};
use snafu::{OptionExt, ResultExt};
use std::fs::File;
use std::io;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use structopt::StructOpt;
use tough::{ExpirationEnforcement, Repository, RepositoryLoader};
use url::Url;

#[derive(Debug, StructOpt)]
pub(crate) struct DownloadArgs {
    /// Path to root.json file for the repository
    #[structopt(short = "r", long = "root")]
    root: Option<PathBuf>,

    /// Remote root.json version number
    #[structopt(short = "v", long = "root-version", default_value = "1")]
    root_version: NonZeroU64,

    /// TUF repository metadata base URL
    #[structopt(short = "m", long = "metadata-url")]
    metadata_base_url: Url,

    /// TUF repository targets base URL
    #[structopt(short = "t", long = "targets-url")]
    targets_base_url: Url,

    /// Allow downloading the root.json file (unsafe)
    #[structopt(long)]
    allow_root_download: bool,

    /// Download only these targets, if specified
    #[structopt(short = "n", long = "target-name")]
    target_names: Vec<String>,

    /// Output directory of targets
    outdir: PathBuf,

    /// Allow repo download for expired metadata
    #[structopt(long)]
    allow_expired_repo: bool,
}

fn expired_repo_warning<P: AsRef<Path>>(path: P) {
    #[rustfmt::skip]
    eprintln!("\
=================================================================
Downloading repo to {}
WARNING: `--allow-expired-repo` was passed; this is unsafe and will not establish trust, use only for testing!
=================================================================",
              path.as_ref().display());
}

impl DownloadArgs {
    pub(crate) fn run(&self) -> Result<()> {
        // use local root.json or download from repository
        let root_path = if let Some(path) = &self.root {
            PathBuf::from(path)
        } else if self.allow_root_download {
            let outdir = std::env::current_dir().context(error::CurrentDir)?;
            download_root(&self.metadata_base_url, self.root_version, outdir)?
        } else {
            eprintln!("No root.json available");
            std::process::exit(1);
        };

        // load repository
        let expiration_enforcement = if self.allow_expired_repo {
            expired_repo_warning(&self.outdir);
            ExpirationEnforcement::Unsafe
        } else {
            ExpirationEnforcement::Safe
        };
        let repository = RepositoryLoader::new(
            File::open(&root_path).context(error::OpenRoot { path: &root_path })?,
            self.metadata_base_url.clone(),
            self.targets_base_url.clone(),
        )
        .expiration_enforcement(expiration_enforcement)
        .load()
        .context(error::RepoLoad)?;

        // download targets
        handle_download(&repository, &self.outdir, &self.target_names)
    }
}

fn handle_download(repository: &Repository, outdir: &Path, target_names: &[String]) -> Result<()> {
    let download_target = |target: &str| -> Result<()> {
        let path = PathBuf::from(outdir).join(target);
        println!("\t-> {}", &target);
        let mut reader = repository
            .read_target(target)
            .context(error::Metadata)?
            .context(error::TargetNotFound { target })?;
        let mut f = File::create(&path).context(error::OpenFile { path: &path })?;
        io::copy(&mut reader, &mut f).context(error::WriteTarget)?;
        Ok(())
    };

    // copy requested targets, or all available targets if not specified
    let targets = if target_names.is_empty() {
        repository
            .targets()
            .signed
            .targets
            .keys()
            .cloned()
            .collect()
    } else {
        target_names.to_owned()
    };

    println!("Downloading targets to {:?}", outdir);
    std::fs::create_dir_all(outdir).context(error::DirCreate { path: outdir })?;
    for target in targets {
        download_target(&target)?;
    }
    Ok(())
}
