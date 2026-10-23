//! Card ownership, matching `scripts/board.py` exactly: a card owns a path
//! when one of its `owns`, `files` or `tests` entries equals it, is a directory
//! prefix of it (ending in `/`), or matches it as a Python `fnmatch` pattern.

use serde_json::Value;

/// Python `fnmatch.fnmatchcase`: `*` (also across `/`), `?`, `[seq]`, `[!seq]`.
pub fn fnmatch(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    matches(&p, &t)
}

fn matches(p: &[char], t: &[char]) -> bool {
    let (mut pi, mut ti) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ti < t.len() {
        if pi < p.len() {
            match p[pi] {
                '*' => {
                    star = Some(pi);
                    mark = ti;
                    pi += 1;
                    continue;
                }
                '?' => {
                    pi += 1;
                    ti += 1;
                    continue;
                }
                '[' => {
                    if let Some((hit, next)) = class(&p[pi..], t[ti]) {
                        if hit {
                            pi += next;
                            ti += 1;
                            continue;
                        }
                    } else if t[ti] == '[' {
                        // An unclosed `[` is a literal, as in Python.
                        pi += 1;
                        ti += 1;
                        continue;
                    }
                }
                c if c == t[ti] => {
                    pi += 1;
                    ti += 1;
                    continue;
                }
                _ => {}
            }
        }
        match star {
            Some(s) => {
                pi = s + 1;
                mark += 1;
                ti = mark;
            }
            None => return false,
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

/// `[...]` at the start of `p`: whether `c` is in it, and the class length.
fn class(p: &[char], c: char) -> Option<(bool, usize)> {
    let mut i = 1;
    let negate = p.get(i) == Some(&'!');
    if negate {
        i += 1;
    }
    let start = i;
    let mut hit = false;
    while i < p.len() {
        if p[i] == ']' && i > start {
            return Some((hit != negate, i + 1));
        }
        if i + 2 < p.len() && p[i + 1] == '-' && p[i + 2] != ']' {
            if p[i] <= c && c <= p[i + 2] {
                hit = true;
            }
            i += 3;
        } else {
            if p[i] == c {
                hit = true;
            }
            i += 1;
        }
    }
    None
}

pub fn owns(node: &Value, path: &str) -> bool {
    let strings = |key: &str| -> Vec<String> {
        node.get(key)
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        item.as_str()
                            .or_else(|| item.get("path").and_then(Value::as_str))
                            .map(str::to_owned)
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    ["owns", "files", "tests"]
        .iter()
        .flat_map(|key| strings(key))
        .filter(|pattern| !pattern.is_empty())
        .any(|pattern| {
            pattern == path
                || (pattern.ends_with('/') && path.starts_with(&pattern))
                || fnmatch(&pattern, path)
        })
}

pub fn owners(board: &Value, path: &str) -> Vec<String> {
    board
        .get("nodes")
        .and_then(Value::as_array)
        .map(|nodes| {
            nodes
                .iter()
                .filter(|node| owns(node, path))
                .filter_map(|node| node.get("id").and_then(Value::as_str).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fnmatch_follows_python() {
        assert!(fnmatch("src/*.py", "src/a.py"));
        assert!(fnmatch("src/*.py", "src/deep/a.py"));
        assert!(fnmatch("a?c", "abc"));
        assert!(!fnmatch("a?c", "ac"));
        assert!(fnmatch("file[0-9].rs", "file7.rs"));
        assert!(!fnmatch("file[!0-9].rs", "file7.rs"));
        assert!(fnmatch("*", ""));
        assert!(fnmatch("a[b", "a[b"));
        assert!(!fnmatch("src/*.py", "lib/a.py"));
    }

    #[test]
    fn ownership_uses_owns_files_and_tests() {
        let board = json!({"nodes": [
            {"id": "app", "owns": ["src/app/"], "files": [{"path": "src/main.py"}], "tests": ["tests/test_app.py"]},
            {"id": "models", "owns": ["src/models/*.py"]}
        ]});
        assert_eq!(owners(&board, "src/app/views.py"), ["app"]);
        assert_eq!(owners(&board, "src/main.py"), ["app"]);
        assert_eq!(owners(&board, "tests/test_app.py"), ["app"]);
        assert_eq!(owners(&board, "src/models/user.py"), ["models"]);
        assert!(owners(&board, "README.md").is_empty());
    }
}
