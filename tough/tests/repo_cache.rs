// Copyright 2019 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use tempfile::TempDir;
use test_utils::{dir_url, test_data};
use tough::{Repository, RepositoryLoader};
use url::Url;

mod test_utils;

struct RepoPaths {
    root_path: PathBuf,
    metadata_base_url: Url,
    targets_base_url: Url,
}

impl RepoPaths {
    fn new() -> Self {
        let base = test_data().join("tuf-reference-impl");
        RepoPaths {
            root_path: base.join("metadata").join("1.root.json"),
            metadata_base_url: dir_url(base.join("metadata")),
            targets_base_url: dir_url(base.join("targets")),
        }
    }

    fn root(&self) -> File {
        File::open(&self.root_path).unwrap()
    }
}

fn load_tuf_reference_impl(paths: &RepoPaths) -> Repository {
    RepositoryLoader::new(
        &mut paths.root(),
        paths.metadata_base_url.clone(),
        paths.targets_base_url.clone(),
    )
    .load()
    .unwrap()
}

/// Test that the repo.cache() function works when given a list of multiple targets.
#[test]
fn test_repo_cache_all_targets() {
    // load the reference_impl repo
    let repo_paths = RepoPaths::new();
    let repo = load_tuf_reference_impl(&repo_paths);

    // cache the repo for future use
    let destination = TempDir::new().unwrap();
    let metadata_destination = destination.as_ref().join("metadata");
    let targets_destination = destination.as_ref().join("targets");
    repo.cache(
        &metadata_destination,
        &targets_destination,
        None::<&[&str]>,
        true,
    )
    .unwrap();

    // check that we can load the copied repo.
    let copied_repo = RepositoryLoader::new(
        repo_paths.root(),
        dir_url(&metadata_destination),
        dir_url(&targets_destination),
    )
    .load()
    .unwrap();

    // the copied repo should have file1 and file2 (i.e. all of targets).
    let mut file_data = Vec::new();
    let file_size = copied_repo
        .read_target("file1.txt")
        .unwrap()
        .unwrap()
        .read_to_end(&mut file_data)
        .unwrap();
    assert_eq!(31, file_size);

    let mut file_data = Vec::new();
    let file_size = copied_repo
        .read_target("file2.txt")
        .unwrap()
        .unwrap()
        .read_to_end(&mut file_data)
        .unwrap();
    assert_eq!(39, file_size);
}

/// Test that the repo.cache() function works when given a list of multiple targets.
#[test]
fn test_repo_cache_list_of_two_targets() {
    // load the reference_impl repo
    let repo_paths = RepoPaths::new();
    let repo = load_tuf_reference_impl(&repo_paths);

    // cache the repo for future use
    let destination = TempDir::new().unwrap();
    let metadata_destination = destination.as_ref().join("metadata");
    let targets_destination = destination.as_ref().join("targets");
    let targets_subset = vec!["file1.txt".to_string(), "file2.txt".to_string()];
    repo.cache(
        &metadata_destination,
        &targets_destination,
        Some(&targets_subset),
        true,
    )
    .unwrap();

    // check that we can load the copied repo.
    let copied_repo = RepositoryLoader::new(
        repo_paths.root(),
        dir_url(&metadata_destination),
        dir_url(&targets_destination),
    )
    .load()
    .unwrap();

    // the copied repo should have file1 and file2 (i.e. all of the listed targets).
    let mut file_data = Vec::new();
    let file_size = copied_repo
        .read_target("file1.txt")
        .unwrap()
        .unwrap()
        .read_to_end(&mut file_data)
        .unwrap();
    assert_eq!(31, file_size);

    let mut file_data = Vec::new();
    let file_size = copied_repo
        .read_target("file2.txt")
        .unwrap()
        .unwrap()
        .read_to_end(&mut file_data)
        .unwrap();
    assert_eq!(39, file_size);
}

/// Test that the repo.cache() function works when given a list of only one of the targets.
#[test]
fn test_repo_cache_some() {
    // load the reference_impl repo
    let repo_paths = RepoPaths::new();
    let repo = load_tuf_reference_impl(&repo_paths);

    // cache the repo for future use
    let destination = TempDir::new().unwrap();
    let metadata_destination = destination.as_ref().join("metadata");
    let targets_destination = destination.as_ref().join("targets");
    let targets_subset = vec!["file2.txt".to_string()];
    repo.cache(
        &metadata_destination,
        &targets_destination,
        Some(&targets_subset),
        true,
    )
    .unwrap();

    // check that we can load the copied repo.
    let copied_repo = RepositoryLoader::new(
        repo_paths.root(),
        dir_url(&metadata_destination),
        dir_url(&targets_destination),
    )
    .load()
    .unwrap();

    // the copied repo should have file2 but not file1 (i.e. only the listed targets).
    let read_target_result = copied_repo.read_target("file1.txt");
    assert!(read_target_result.is_err());

    let mut file_data = Vec::new();
    let file_size = copied_repo
        .read_target("file2.txt")
        .unwrap()
        .unwrap()
        .read_to_end(&mut file_data)
        .unwrap();
    assert_eq!(39, file_size);
}

#[test]
fn test_repo_cache_metadata() {
    // Load the reference_impl repo
    let repo_paths = RepoPaths::new();
    let repo = load_tuf_reference_impl(&repo_paths);

    // Cache the repo for future use
    let destination = TempDir::new().unwrap();
    let metadata_destination = destination.as_ref().join("metadata");
    repo.cache_metadata(&metadata_destination, true).unwrap();

    // Load the copied repo - this validates we cached the metadata (if we didn't we couldn't load
    // the repo)
    let targets_destination = destination.as_ref().join("targets");
    let copied_repo = RepositoryLoader::new(
        repo_paths.root(),
        dir_url(&metadata_destination),
        dir_url(&targets_destination),
    )
    .load()
    .unwrap();

    // Validate we didn't cache any targets
    for (target_name, _) in copied_repo.targets().signed.targets_map() {
        assert!(copied_repo.read_target(&target_name).is_err())
    }

    // Verify we also loaded the delegated role "role1"
    let read_delegated_role_option = copied_repo.delegated_role("role1");
    assert!(read_delegated_role_option.is_some());

    // Verify we cached the root.json
    assert!(metadata_destination.join("1.root.json").exists());
}

#[test]
fn test_repo_cache_metadata_no_root_chain() {
    // Load the reference_impl repo
    let repo_paths = RepoPaths::new();
    let repo = load_tuf_reference_impl(&repo_paths);

    // Cache the repo for future use
    let destination = TempDir::new().unwrap();
    let metadata_destination = destination.as_ref().join("metadata");
    repo.cache_metadata(&metadata_destination, false).unwrap();

    // Verify we did not cache the root.json
    assert!(!metadata_destination.join("1.root.json").exists());
}
