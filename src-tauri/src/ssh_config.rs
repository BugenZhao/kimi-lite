//! Concrete `Host` aliases from `~/.ssh/config` (following `Include`), for
//! the host picker. Wildcard and negated patterns are skipped.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub fn list_hosts() -> Vec<String> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return Vec::new() };
    let mut hosts = Vec::new();
    let mut seen = HashSet::new();
    let mut visited = HashSet::new();
    collect(&home.join(".ssh/config"), &home, &mut hosts, &mut seen, &mut visited, 0);
    hosts
}

fn collect(
    path: &Path,
    home: &Path,
    hosts: &mut Vec<String>,
    seen: &mut HashSet<String>,
    visited: &mut HashSet<PathBuf>,
    depth: usize,
) {
    if depth > 8 || !visited.insert(path.to_path_buf()) {
        return;
    }
    let Ok(text) = std::fs::read_to_string(path) else { return };
    for (keyword, args) in text.lines().filter_map(directive) {
        if keyword.eq_ignore_ascii_case("host") {
            hosts.extend(parse_host_patterns(args).into_iter().filter(|h| seen.insert(h.clone())));
        } else if keyword.eq_ignore_ascii_case("include") {
            for pattern in args.split_whitespace() {
                for include in expand_include(pattern, home) {
                    collect(&include, home, hosts, seen, visited, depth + 1);
                }
            }
        }
    }
}

fn directive(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let split = line.find(|c: char| c.is_whitespace() || c == '=')?;
    let (keyword, rest) = line.split_at(split);
    Some((keyword, rest.trim_start_matches(|c: char| c.is_whitespace() || c == '=')))
}

fn parse_host_patterns(args: &str) -> Vec<String> {
    args.split_whitespace()
        .take_while(|t| !t.starts_with('#'))
        .filter(|t| !t.contains(['*', '?', '!']))
        .map(str::to_string)
        .collect()
}

/// Relative includes resolve against `~/.ssh`; a `*` in the file name globs.
fn expand_include(pattern: &str, home: &Path) -> Vec<PathBuf> {
    let path = if let Some(rest) = pattern.strip_prefix("~/") {
        home.join(rest)
    } else if Path::new(pattern).is_absolute() {
        PathBuf::from(pattern)
    } else {
        home.join(".ssh").join(pattern)
    };
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
    if !name.contains('*') {
        return vec![path];
    }
    let (prefix, suffix) = name.split_once('*').unwrap();
    let Some(dir) = path.parent() else { return Vec::new() };
    let mut matches: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(prefix) && n.ends_with(suffix) && n.len() >= prefix.len() + suffix.len())
        })
        .collect();
    matches.sort();
    matches
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_patterns() {
        assert_eq!(parse_host_patterns("gpu-1 gpu-2 *.corp !bastion # note"), vec!["gpu-1", "gpu-2"]);
        assert_eq!(directive("Host=box"), Some(("Host", "box")));
        assert_eq!(directive("  # comment"), None);
    }
}
