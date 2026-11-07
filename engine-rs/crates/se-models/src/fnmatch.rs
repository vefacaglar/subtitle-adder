//! Python `fnmatch.fnmatchcase` and huggingface_hub `filter_repo_objects(allow_patterns=...)`.

/// `fnmatch.fnmatchcase(name, pattern)`: `*` matches anything (including `/`), `?` one character, `[seq]` /
/// `[!seq]` a set (ranges allowed); an unclosed `[` is a literal. Case-sensitive.
pub fn fnmatchcase(name: &str, pattern: &str) -> bool {
    let n: Vec<char> = name.chars().collect();
    let p: Vec<char> = pattern.chars().collect();
    matches(&n, &p)
}

fn matches(n: &[char], p: &[char]) -> bool {
    // Iterative wildcard matching with backtracking on the last `*`.
    let (mut ni, mut pi) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    loop {
        if pi < p.len() {
            match p[pi] {
                '*' => {
                    star = Some((pi, ni));
                    pi += 1;
                    continue;
                }
                '?' if ni < n.len() => {
                    ni += 1;
                    pi += 1;
                    continue;
                }
                '[' => {
                    if let Some((set_end, matched)) = class(p, pi, n.get(ni).copied()) {
                        if ni < n.len() && matched {
                            ni += 1;
                            pi = set_end;
                            continue;
                        }
                    } else if ni < n.len() && n[ni] == '[' {
                        ni += 1;
                        pi += 1;
                        continue;
                    }
                }
                c if c != '?' && ni < n.len() && n[ni] == c => {
                    ni += 1;
                    pi += 1;
                    continue;
                }
                _ => {}
            }
        } else if ni == n.len() {
            return true;
        }
        match star {
            Some((sp, sn)) if sn < n.len() => {
                star = Some((sp, sn + 1));
                pi = sp + 1;
                ni = sn + 1;
            }
            _ => return false,
        }
    }
}

/// Parses `[...]` at `p[start]`; returns (index after `]`, whether `c` is in the set) or `None` when unclosed.
fn class(p: &[char], start: usize, c: Option<char>) -> Option<(usize, bool)> {
    let mut i = start + 1;
    let negate = p.get(i) == Some(&'!');
    if negate {
        i += 1;
    }
    // A `]` right after `[` or `[!` is a literal member.
    let first = i;
    let mut members: Vec<(char, char)> = Vec::new();
    while i < p.len() && (p[i] != ']' || i == first) {
        if i + 2 < p.len() && p[i + 1] == '-' && p[i + 2] != ']' {
            members.push((p[i], p[i + 2]));
            i += 3;
        } else {
            members.push((p[i], p[i]));
            i += 1;
        }
    }
    if i >= p.len() {
        return None;
    }
    let hit = c.is_some_and(|c| members.iter().any(|(lo, hi)| *lo <= c && c <= *hi));
    Some((i + 1, hit != negate))
}

/// `filter_repo_objects(items, allow_patterns=patterns)` (no ignore patterns): pattern `\` → `/`, a trailing
/// `/` gets `*`; keeps the items matching any pattern, in order.
pub fn filter_repo_objects<'a>(items: &'a [String], patterns: &[&str]) -> Vec<&'a String> {
    let patterns: Vec<String> = patterns
        .iter()
        .map(|p| {
            let p = p.replace('\\', "/");
            if p.ends_with('/') { format!("{p}*") } else { p }
        })
        .collect();
    items.iter().filter(|item| patterns.iter().any(|p| fnmatchcase(item, p))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnmatch_cases() {
        assert!(fnmatchcase("vocabulary.txt", "vocabulary.*"));
        assert!(fnmatchcase("vocabulary.json", "vocabulary.*"));
        assert!(!fnmatchcase("sub/vocabulary.txt", "vocabulary.*"));
        assert!(fnmatchcase("a/b/c.bin", "*.bin"));
        assert!(fnmatchcase("model.bin", "model.bin"));
        assert!(!fnmatchcase("Model.bin", "model.bin"));
        assert!(fnmatchcase("x1", "x[0-9]"));
        assert!(!fnmatchcase("xa", "x[0-9]"));
        assert!(fnmatchcase("xa", "x[!0-9]"));
        assert!(fnmatchcase("x[", "x["));
        assert!(fnmatchcase("abc", "a?c"));
        assert!(fnmatchcase("", "*"));
        assert!(fnmatchcase("aXbYc", "a*b*c"));
        assert!(!fnmatchcase("aXbY", "a*b*c"));
    }

    #[test]
    fn filter_keeps_order() {
        let items: Vec<String> = ["config.json", "model.bin", "README.md", "vocabulary.txt"].map(String::from).to_vec();
        let got = filter_repo_objects(&items, crate::catalog::ALLOW_PATTERNS);
        assert_eq!(got, vec!["config.json", "model.bin", "vocabulary.txt"]);
    }
}
