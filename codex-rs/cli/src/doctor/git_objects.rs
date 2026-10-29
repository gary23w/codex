//! Inspects the Git object store of the working directory's repository
//! without running Git.
//!
//! Codex writes snapshot trees and `refs/codex/*` refs into the repository as
//! it works. Nothing packs or prunes those objects unless `git gc` or
//! `git maintenance` runs, and a write interrupted part-way leaves `tmp_obj_*`
//! and `tmp_pack_*` files behind that `git gc` reports but never removes. This
//! check measures all of that from the filesystem and reports whether the
//! repository is registered for scheduled maintenance.

use std::fs;
use std::io;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

use codex_git_utils::get_git_repo_root;

use super::CheckStatus;
use super::DoctorCheck;
use super::DoctorIssue;

/// Git's own `gc.auto` default. `git gc --auto` packs once this many loose
/// objects exist, but only after commands that invoke it, and snapshot writes
/// never do.
const LOOSE_OBJECT_WARN_COUNT: u64 = 6_700;
/// Loose objects above this size slow every status, diff and snapshot command
/// and are far beyond what a healthy repository accumulates between packs.
const LOOSE_OBJECT_WARN_BYTES: u64 = 200 * 1024 * 1024;
const CODEX_REF_PREFIX: &str = "refs/codex/";

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ObjectStoreStats {
    loose_count: u64,
    loose_bytes: u64,
    pack_count: u64,
    pack_bytes: u64,
    garbage_count: u64,
    garbage_bytes: u64,
    codex_ref_count: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum MaintenanceRegistration {
    Registered,
    NotRegistered,
    #[default]
    Unknown,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ObjectStoreInputs {
    repo_root: Option<PathBuf>,
    common_dir: Option<PathBuf>,
    stats: Option<ObjectStoreStats>,
    inspect_error: Option<String>,
    maintenance: MaintenanceRegistration,
}

pub(super) fn check(cwd: &Path) -> DoctorCheck {
    let Some(repo_root) = get_git_repo_root(cwd) else {
        return check_from_inputs(ObjectStoreInputs::default());
    };
    let mut inputs = ObjectStoreInputs {
        repo_root: Some(repo_root.clone()),
        ..ObjectStoreInputs::default()
    };
    match resolve_common_dir(&repo_root) {
        Ok(common_dir) => {
            match collect_stats(&common_dir) {
                Ok(stats) => inputs.stats = Some(stats),
                Err(err) => inputs.inspect_error = Some(err.to_string()),
            }
            // A linked worktree is registered under its main worktree, which
            // is the parent of the common git directory.
            let candidates = std::iter::once(repo_root)
                .chain(common_dir.parent().map(Path::to_path_buf))
                .collect::<Vec<_>>();
            inputs.maintenance = maintenance_registration(&global_config_files(), &candidates);
            inputs.common_dir = Some(common_dir);
        }
        Err(err) => inputs.inspect_error = Some(err.to_string()),
    }
    check_from_inputs(inputs)
}

fn check_from_inputs(inputs: ObjectStoreInputs) -> DoctorCheck {
    let Some(repo_root) = inputs.repo_root.as_deref() else {
        return DoctorCheck::new(
            "git.object_store",
            "git objects",
            CheckStatus::Ok,
            "no git repository in the working directory",
        )
        .detail("repo detected: false");
    };

    let mut details = vec![format!("repo root: {}", repo_root.display())];
    if let Some(common_dir) = inputs.common_dir.as_deref() {
        details.push(format!("common git dir: {}", common_dir.display()));
    }
    let Some(stats) = inputs.stats.as_ref() else {
        let reason = inputs
            .inspect_error
            .clone()
            .unwrap_or_else(|| "unknown error".to_string());
        details.push(format!("inspection: skipped ({reason})"));
        return DoctorCheck::new(
            "git.object_store",
            "git objects",
            CheckStatus::Ok,
            "git object store not inspected",
        )
        .details(details);
    };

    details.push(format!(
        "loose objects: {} ({})",
        stats.loose_count,
        format_bytes(stats.loose_bytes)
    ));
    details.push(format!(
        "pack files: {} ({})",
        stats.pack_count,
        format_bytes(stats.pack_bytes)
    ));
    details.push(format!(
        "garbage files: {} ({})",
        stats.garbage_count,
        format_bytes(stats.garbage_bytes)
    ));
    details.push(format!("codex refs: {}", stats.codex_ref_count));
    details.push(format!(
        "git maintenance: {}",
        match inputs.maintenance {
            MaintenanceRegistration::Registered => "registered in global config",
            MaintenanceRegistration::NotRegistered => "not registered in global config",
            MaintenanceRegistration::Unknown => "unknown (global config not readable)",
        }
    ));

    let mut check = DoctorCheck::new(
        "git.object_store",
        "git objects",
        CheckStatus::Ok,
        "git object store is healthy",
    )
    .details(details);

    if stats.garbage_count > 0 {
        check = check.issue(
            DoctorIssue::new(
                CheckStatus::Warning,
                "interrupted git writes left temporary object files behind",
            )
            .measured(format!(
                "{} files ({})",
                stats.garbage_count,
                format_bytes(stats.garbage_bytes)
            ))
            .expected("0 files")
            .remedy(
                "Delete tmp_obj_* and tmp_pack_* files under .git/objects that are no longer \
                 being written; git gc reports them as garbage but does not remove them.",
            )
            .field("garbage files"),
        );
    }

    if stats.loose_count > LOOSE_OBJECT_WARN_COUNT || stats.loose_bytes > LOOSE_OBJECT_WARN_BYTES {
        let remedy = match inputs.maintenance {
            MaintenanceRegistration::Registered => {
                "Scheduled maintenance is registered but has not packed these objects yet; \
                 run git maintenance run --task=gc, or git gc, once."
            }
            MaintenanceRegistration::NotRegistered | MaintenanceRegistration::Unknown => {
                "Run git maintenance register and git maintenance start in this repository \
                 so git packs and prunes on a schedule, or run git gc once."
            }
        };
        check = check.issue(
            DoctorIssue::new(
                CheckStatus::Warning,
                "loose objects exceed the automatic gc threshold",
            )
            .measured(format!(
                "{} objects ({})",
                stats.loose_count,
                format_bytes(stats.loose_bytes)
            ))
            .expected(format!(
                "under {LOOSE_OBJECT_WARN_COUNT} objects and {}",
                format_bytes(LOOSE_OBJECT_WARN_BYTES)
            ))
            .remedy(remedy)
            .field("loose objects"),
        );
    }

    if !check.issues.is_empty() {
        check.status = CheckStatus::Warning;
        check.summary = "git object store needs maintenance".to_string();
    }

    check
}

/// Resolves the directory that holds `objects/` and `refs/` for the repository
/// at `repo_root`, following a `.git` file for linked worktrees and the
/// `commondir` indirection inside their private git directories.
fn resolve_common_dir(repo_root: &Path) -> io::Result<PathBuf> {
    let entry = repo_root.join(".git");
    let git_dir = if fs::metadata(&entry)?.is_dir() {
        entry
    } else {
        let contents = fs::read_to_string(&entry)?;
        let target = contents
            .strip_prefix("gitdir:")
            .map(str::trim)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, ".git file has no gitdir entry")
            })?;
        resolve_relative(repo_root, target)
    };
    match fs::read_to_string(git_dir.join("commondir")) {
        Ok(contents) => Ok(resolve_relative(&git_dir, contents.trim())),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(git_dir),
        Err(err) => Err(err),
    }
}

fn resolve_relative(base: &Path, target: &str) -> PathBuf {
    let target = Path::new(target);
    if target.is_absolute() {
        target.to_path_buf()
    } else {
        base.join(target)
    }
}

/// Counts loose objects, pack files, leftover temporary files and
/// `refs/codex/*` refs. Entries that cannot be read are skipped so one
/// unreadable file does not hide the rest of the picture.
fn collect_stats(common_dir: &Path) -> io::Result<ObjectStoreStats> {
    let mut stats = ObjectStoreStats::default();
    for entry in fs::read_dir(common_dir.join("objects"))?.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == "pack" {
            for (file_name, size) in files_in(&entry.path()) {
                if file_name.starts_with("tmp_pack_") || file_name.starts_with("tmp_idx_") {
                    stats.garbage_count += 1;
                    stats.garbage_bytes += size;
                } else if file_name.ends_with(".pack") {
                    stats.pack_count += 1;
                    stats.pack_bytes += size;
                }
            }
        } else if is_object_fanout_dir(&name) {
            for (file_name, size) in files_in(&entry.path()) {
                if file_name.starts_with("tmp_obj_") {
                    stats.garbage_count += 1;
                    stats.garbage_bytes += size;
                } else {
                    stats.loose_count += 1;
                    stats.loose_bytes += size;
                }
            }
        }
    }
    stats.codex_ref_count = count_codex_refs(common_dir)?;
    Ok(stats)
}

fn is_object_fanout_dir(name: &str) -> bool {
    name.len() == 2 && name.chars().all(|c| c.is_ascii_hexdigit())
}

fn files_in(dir: &Path) -> Vec<(String, u64)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            metadata.is_file().then(|| {
                (
                    entry.file_name().to_string_lossy().into_owned(),
                    metadata.len(),
                )
            })
        })
        .collect()
}

fn count_codex_refs(common_dir: &Path) -> io::Result<u64> {
    let mut count = count_files(&common_dir.join("refs").join("codex"))?;
    match fs::read_to_string(common_dir.join("packed-refs")) {
        Ok(contents) => {
            count += contents
                .lines()
                .filter(|line| {
                    packed_ref_name(line).is_some_and(|name| name.starts_with(CODEX_REF_PREFIX))
                })
                .count() as u64;
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    Ok(count)
}

fn packed_ref_name(line: &str) -> Option<&str> {
    if line.starts_with('#') || line.starts_with('^') {
        return None;
    }
    line.split_once(' ').map(|(_, name)| name.trim())
}

fn count_files(dir: &Path) -> io::Result<u64> {
    let mut stack = vec![dir.to_path_buf()];
    let mut count = 0;
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => return Err(err),
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                stack.push(entry.path());
            } else if file_type.is_file() {
                count += 1;
            }
        }
    }
    Ok(count)
}

/// The global git config files `git maintenance register` writes to, in the
/// order git reads them. Includes are not followed; the result is best effort.
fn global_config_files() -> Vec<PathBuf> {
    if let Some(path) = std::env::var_os("GIT_CONFIG_GLOBAL") {
        return vec![PathBuf::from(path)];
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    let mut files = Vec::new();
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|home| home.join(".config")));
    if let Some(xdg) = xdg {
        files.push(xdg.join("git").join("config"));
    }
    if let Some(home) = home {
        files.push(home.join(".gitconfig"));
    }
    files
}

fn maintenance_registration(
    config_files: &[PathBuf],
    candidates: &[PathBuf],
) -> MaintenanceRegistration {
    let wanted = candidates
        .iter()
        .map(|path| normalize_path(path))
        .collect::<Vec<_>>();
    let mut any_readable = false;
    for file in config_files {
        let Ok(contents) = fs::read_to_string(file) else {
            continue;
        };
        any_readable = true;
        let registered = config_values(&contents, "maintenance", "repo")
            .iter()
            .any(|value| wanted.contains(&normalize_path(Path::new(value))));
        if registered {
            return MaintenanceRegistration::Registered;
        }
    }
    if any_readable {
        MaintenanceRegistration::NotRegistered
    } else {
        MaintenanceRegistration::Unknown
    }
}

/// Returns every value of `key` under a plain `[section]` header. Subsections,
/// includes and conditional includes are ignored on purpose: this only needs to
/// recognise what `git maintenance register` writes.
fn config_values(contents: &str, section: &str, key: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut in_section = false;
    for raw in contents.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(header) = line
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            in_section = header.trim().eq_ignore_ascii_case(section);
            continue;
        }
        if !in_section {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case(key) {
            let value = value.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|rest| rest.strip_suffix('"'))
                .unwrap_or(value);
            values.push(value.to_string());
        }
    }
    values
}

/// Lexically normalised, absolute, forward-slash form used to compare the
/// paths git stores in config against paths resolved from the filesystem.
fn normalize_path(path: &Path) -> String {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut out = String::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => out.push_str(&prefix.as_os_str().to_string_lossy()),
            Component::RootDir => out.push('/'),
            Component::CurDir => {}
            Component::ParentDir => {
                let trimmed = out.trim_end_matches('/').to_string();
                match trimmed.rfind('/') {
                    Some(index) => out.truncate(index + 1),
                    None => out = trimmed,
                }
            }
            Component::Normal(part) => {
                if !out.is_empty() && !out.ends_with('/') {
                    out.push('/');
                }
                out.push_str(&part.to_string_lossy());
            }
        }
    }
    let out = if out.len() > 1 {
        out.trim_end_matches('/').to_string()
    } else {
        out
    };
    if cfg!(windows) {
        out.to_ascii_lowercase()
    } else {
        out
    }
}

fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let value = bytes as f64;
    if value >= GIB {
        format!("{:.1} GiB", value / GIB)
    } else if value >= MIB {
        format!("{:.1} MiB", value / MIB)
    } else if value >= KIB {
        format!("{:.1} KiB", value / KIB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn write(path: &Path, bytes: usize) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![b'x'; bytes]).unwrap();
    }

    #[test]
    fn collects_loose_pack_garbage_and_codex_refs() {
        let temp = tempfile::tempdir().unwrap();
        let git_dir = temp.path().join(".git");
        let objects = git_dir.join("objects");
        write(&objects.join("ab").join("cdef"), 10);
        write(&objects.join("ab").join("tmp_obj_abc"), 7);
        write(&objects.join("ff").join("0001"), 5);
        write(&objects.join("pack").join("pack-1.pack"), 100);
        write(&objects.join("pack").join("pack-1.idx"), 20);
        write(&objects.join("pack").join("tmp_pack_xyz"), 30);
        write(&objects.join("info").join("alternates"), 1);
        let refs = git_dir.join("refs");
        write(
            &refs
                .join("codex")
                .join("turn-diffs")
                .join("checkpoints")
                .join("a"),
            41,
        );
        write(
            &refs
                .join("codex")
                .join("turn-diffs")
                .join("captures")
                .join("b"),
            41,
        );
        write(&refs.join("heads").join("main"), 41);
        fs::write(
            git_dir.join("packed-refs"),
            "# pack-refs with: peeled fully-peeled sorted \n\
             0000000000000000000000000000000000000001 refs/codex/turn-diffs/checkpoints/x/y\n\
             ^0000000000000000000000000000000000000002\n\
             0000000000000000000000000000000000000003 refs/heads/main\n\
             0000000000000000000000000000000000000004 refs/codex/other\n",
        )
        .unwrap();

        let stats = collect_stats(&git_dir).unwrap();

        assert_eq!(
            stats,
            ObjectStoreStats {
                loose_count: 2,
                loose_bytes: 15,
                pack_count: 1,
                pack_bytes: 100,
                garbage_count: 2,
                garbage_bytes: 37,
                codex_ref_count: 4,
            }
        );
    }

    #[test]
    fn resolves_linked_worktree_common_dir() {
        let temp = tempfile::tempdir().unwrap();
        let main_git = temp.path().join("main").join(".git");
        let worktree_git = main_git.join("worktrees").join("feature");
        fs::create_dir_all(&worktree_git).unwrap();
        fs::write(worktree_git.join("commondir"), "../..\n").unwrap();
        let linked = temp.path().join("feature");
        fs::create_dir_all(&linked).unwrap();
        fs::write(
            linked.join(".git"),
            format!("gitdir: {}\n", worktree_git.display()),
        )
        .unwrap();

        let common_dir = resolve_common_dir(&linked).unwrap();

        assert_eq!(normalize_path(&common_dir), normalize_path(&main_git));
    }

    #[test]
    fn registration_matches_normalized_paths() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("gitconfig");
        let repo = temp.path().join("Repo");
        let registered = repo.display().to_string().replace('\\', "/");
        fs::write(
            &config,
            format!(
                "[core]\n\tautocrlf = false\n[maintenance]\n\trepo = {registered}\n\trepo = /elsewhere\n"
            ),
        )
        .unwrap();

        assert_eq!(
            maintenance_registration(std::slice::from_ref(&config), std::slice::from_ref(&repo)),
            MaintenanceRegistration::Registered
        );
        assert_eq!(
            maintenance_registration(std::slice::from_ref(&config), &[temp.path().join("Other")]),
            MaintenanceRegistration::NotRegistered
        );
        assert_eq!(
            maintenance_registration(&[temp.path().join("missing")], std::slice::from_ref(&repo)),
            MaintenanceRegistration::Unknown
        );
    }

    #[test]
    fn reports_no_repository() {
        let check = check_from_inputs(ObjectStoreInputs::default());

        assert_eq!(check.status, CheckStatus::Ok);
        assert_eq!(check.summary, "no git repository in the working directory");
    }

    #[test]
    fn healthy_store_is_ok() {
        let check = check_from_inputs(ObjectStoreInputs {
            repo_root: Some(PathBuf::from("/repo")),
            common_dir: Some(PathBuf::from("/repo/.git")),
            stats: Some(ObjectStoreStats {
                loose_count: 12,
                loose_bytes: 4096,
                pack_count: 1,
                pack_bytes: 1 << 20,
                ..ObjectStoreStats::default()
            }),
            maintenance: MaintenanceRegistration::Registered,
            inspect_error: None,
        });

        assert_eq!(check.status, CheckStatus::Ok);
        assert_eq!(check.summary, "git object store is healthy");
        assert!(check.issues.is_empty());
    }

    #[test]
    fn warns_on_garbage_and_loose_object_bloat() {
        let check = check_from_inputs(ObjectStoreInputs {
            repo_root: Some(PathBuf::from("/repo")),
            common_dir: Some(PathBuf::from("/repo/.git")),
            stats: Some(ObjectStoreStats {
                loose_count: 25_605,
                loose_bytes: 605 * 1024 * 1024,
                pack_count: 0,
                pack_bytes: 0,
                garbage_count: 11,
                garbage_bytes: 68 * 1024 * 1024,
                codex_ref_count: 83,
            }),
            maintenance: MaintenanceRegistration::NotRegistered,
            inspect_error: None,
        });

        assert_eq!(check.status, CheckStatus::Warning);
        assert_eq!(check.summary, "git object store needs maintenance");
        assert_eq!(check.issues.len(), 2);
        assert_eq!(
            check.issues[0].measured.as_deref(),
            Some("11 files (68.0 MiB)")
        );
        assert_eq!(
            check.issues[1].measured.as_deref(),
            Some("25605 objects (605.0 MiB)")
        );
        assert_eq!(
            check.issues[1].expected.as_deref(),
            Some("under 6700 objects and 200.0 MiB")
        );
        assert!(
            check.details.contains(&"codex refs: 83".to_string()),
            "{:?}",
            check.details
        );
        assert!(
            check
                .details
                .contains(&"git maintenance: not registered in global config".to_string()),
            "{:?}",
            check.details
        );
    }
}
