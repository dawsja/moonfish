//! SSH helpers that need no connection: quoting for the remote shell and the hosts named in
//! the user's OpenSSH config.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

fn user_config_path() -> PathBuf {
    crate::paths::home_dir().join(".ssh").join("config")
}

fn config_target(path: &Path) -> crate::Result<PathBuf> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(fs::canonicalize(path)?),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(path.to_owned()),
        Err(e) => Err(e.into()),
    }
}

fn read_config(path: &Path) -> crate::Result<Option<String>> {
    match fs::metadata(path) {
        Ok(meta) if meta.is_file() => Ok(Some(fs::read_to_string(path)?)),
        Ok(_) => Err(crate::Error::Invalid(format!("{} is not a regular file", path.display()))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Loads the main SSH config. Missing files are represented separately from empty files.
pub fn read_user_config() -> crate::Result<Option<String>> {
    read_config(&config_target(&user_config_path())?)
}

/// Saves the main SSH config only if it still matches what the editor loaded.
pub fn save_user_config(original: Option<&str>, text: &str) -> crate::Result<()> {
    static WRITER: Mutex<()> = Mutex::new(());
    let _writer = WRITER.lock().unwrap();
    save_config(&user_config_path(), original, text)
}

fn save_config(path: &Path, original: Option<&str>, text: &str) -> crate::Result<()> {
    if text.contains('\0') {
        return Err(crate::Error::Invalid("SSH config cannot contain NUL characters.".into()));
    }
    let target = config_target(path)?;
    let check_original = || -> crate::Result<()> {
        if config_target(path)? != target || read_config(&target)?.as_deref() != original {
            return Err(crate::Error::Invalid("SSH config changed on disk. Reload it before saving.".into()));
        }
        Ok(())
    };
    check_original()?;
    let dir = target.parent().ok_or_else(|| crate::Error::Invalid("SSH config path has no parent directory.".into()))?;
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)?;
    let mut temp = tempfile::NamedTempFile::new_in(dir)?;
    temp.write_all(text.as_bytes())?;
    temp.as_file().sync_all()?;
    check_original()?;
    if original.is_some() {
        temp.persist(&target).map_err(|e| e.error)?;
    } else {
        temp.persist_noclobber(&target).map_err(|e| e.error)?;
    }
    Ok(())
}

/// POSIX shell single-quoting.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// A host name ssh won't read as an option, with no whitespace or control characters.
pub fn valid_host(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('-') && !name.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// `Host` names from an OpenSSH client config (patterns skipped).
pub fn config_hosts(config: &str) -> Vec<String> {
    let mut hosts = Vec::new();
    for line in config.lines() {
        let line = line.trim_start();
        let Some(split) = line.find(|c: char| c == '=' || c.is_whitespace()) else { continue };
        let (key, tail) = line.split_at(split);
        if !key.eq_ignore_ascii_case("host") {
            continue;
        }
        let tail = tail.trim_start();
        let tail = tail.strip_prefix('=').unwrap_or(tail);
        for h in tail.split_whitespace() {
            if h.starts_with('#') {
                break;
            }
            let h = h.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(h);
            if valid_host(h) && !h.contains(['*', '?', '!', '"']) && !hosts.iter().any(|x| x == h) {
                hosts.push(h.to_string());
            }
        }
    }
    hosts
}

/// The hosts in `~/.ssh/config` and the files it `Include`s (empty when there's no config).
pub fn user_config_hosts() -> Vec<String> {
    let file = user_config_path();
    let mut hosts = Vec::new();
    if let Some(dir) = file.parent() {
        collect(dir, &file, &mut hosts, 0);
    }
    hosts
}

fn collect(ssh_dir: &std::path::Path, file: &std::path::Path, hosts: &mut Vec<String>, depth: u8) {
    let Ok(text) = std::fs::read_to_string(file) else { return };
    for h in config_hosts(&text) {
        if !hosts.contains(&h) {
            hosts.push(h);
        }
    }
    if depth > 4 {
        return;
    }
    for inc in includes(&text) {
        let inc = inc.strip_prefix("~/").map(|r| crate::paths::home_dir().join(r)).unwrap_or_else(|| ssh_dir.join(&inc));
        // `Include dir/*` (a trailing wildcard) reads every file in the folder.
        let name = inc.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if name.contains('*') {
            let prefix = name.split('*').next().unwrap_or("").to_string();
            if let Some(parent) = inc.parent() {
                let mut files: Vec<_> = std::fs::read_dir(parent)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_file() && p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(&prefix)))
                    .collect();
                files.sort();
                for f in files {
                    collect(ssh_dir, &f, hosts, depth + 1);
                }
            }
        } else {
            collect(ssh_dir, &inc, hosts, depth + 1);
        }
    }
}

/// `Include` targets in a config.
pub fn includes(config: &str) -> Vec<String> {
    config
        .lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            let k = w.next()?;
            k.eq_ignore_ascii_case("include").then(|| w.take_while(|x| !x.starts_with('#')).map(String::from).collect::<Vec<_>>())
        })
        .flatten()
        .collect()
}

/// `~/…` for paths under `home` (a remote home, `/`-separated).
pub fn short_with_home(p: &str, home: &str) -> String {
    let home = home.trim_end_matches('/');
    if home.is_empty() {
        return p.to_string();
    }
    if p == home {
        return "~".into();
    }
    match p.strip_prefix(home) {
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => p.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_the_config_and_rejects_stale_saves() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".ssh").join("config");
        let initial = "# my config\nHost dev\n  HostName dev.example.test\n";
        let edited = format!("{initial}\nHost build\n  Port 2222\n");
        assert_eq!(read_config(&path).unwrap(), None);
        save_config(&path, None, initial).unwrap();
        assert!(save_config(&path, None, "").is_err());
        save_config(&path, Some(initial), &edited).unwrap();
        assert!(save_config(&path, Some(initial), "").is_err());
        assert_eq!(read_config(&path).unwrap().as_deref(), Some(edited.as_str()));
    }

    #[cfg(unix)]
    #[test]
    fn edits_a_symlink_target_with_private_permissions() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("actual-config");
        let path = dir.path().join("config");
        fs::write(&target, "Host old\n").unwrap();
        symlink(&target, &path).unwrap();
        save_config(&path, Some("Host old\n"), "Host new\n").unwrap();
        assert!(fs::symlink_metadata(&path).unwrap().is_symlink());
        assert_eq!(fs::read_to_string(&target).unwrap(), "Host new\n");
        assert_eq!(fs::metadata(&target).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn reads_hosts_from_a_config() {
        let cfg = "Host devbox box2 # two\n  HostName 10.0.0.2\nHost *.corp !x\nhost   gpu\nMatch all\nHost devbox\nHost=\"build\"\n";
        assert_eq!(config_hosts(cfg), ["devbox", "box2", "gpu", "build"]);
    }

    #[test]
    fn follows_includes() {
        assert_eq!(
            includes(
                "# x
Include gond/config
  include a b # c
Host x"
            ),
            ["gond/config", "a", "b"]
        );
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(
            dir.path().join("config"),
            "Include sub/*
Host top
",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("sub").join("one"),
            "Host ditto
",
        )
        .unwrap();
        let mut hosts = Vec::new();
        collect(dir.path(), &dir.path().join("config"), &mut hosts, 0);
        assert_eq!(hosts, ["top", "ditto"]);
    }

    #[test]
    fn quotes_and_validates() {
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
        assert!(valid_host("dev-box.local"));
        assert!(!valid_host("-oProxyCommand=x"));
        assert!(!valid_host("a b"));
        assert_eq!(short_with_home("/home/u/x", "/home/u/"), "~/x");
        assert_eq!(short_with_home("/home/u", "/home/u"), "~");
        assert_eq!(short_with_home("/home/uu", "/home/u"), "/home/uu");
    }
}
