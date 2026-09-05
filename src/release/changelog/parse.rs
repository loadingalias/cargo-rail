//! Conventional commit parsing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParsedSubject<'a> {
    pub commit_type: Option<&'a str>,
    pub scope: Option<&'a str>,
    pub breaking: bool,
    pub description: &'a str,
}
impl ParsedSubject<'_> {
    #[expect(dead_code, reason = "parser API used by release diagnostics")]
    pub fn is_conventional(&self) -> bool {
        self.commit_type.is_some()
    }
}
pub fn parse_subject<'a>(subject: &'a str, body: Option<&str>) -> ParsedSubject<'a> {
    let breaking_body = body.is_some_and(|b| b.contains("BREAKING CHANGE:") || b.contains("BREAKING-CHANGE:"));
    let Some((head, description)) = subject.split_once(':') else {
        return ParsedSubject {
            commit_type: None,
            scope: None,
            breaking: breaking_body,
            description: subject,
        };
    };
    let mut token = head.trim();
    let breaking = token.ends_with('!');
    if breaking {
        token = token.strip_suffix('!').unwrap_or(token).trim_end();
    }
    let (ty, scope) = if let Some(open) = token.find('(') {
        if token.ends_with(')') {
            let (ty, scope_part) = token.split_at(open);
            (ty, scope_part.get(1..scope_part.len() - 1))
        } else {
            return ParsedSubject {
                commit_type: None,
                scope: None,
                breaking: breaking_body,
                description: subject,
            };
        }
    } else {
        (token, None)
    };
    if ty.is_empty() || !ty.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return ParsedSubject {
            commit_type: None,
            scope: None,
            breaking: breaking_body,
            description: subject,
        };
    }
    ParsedSubject {
        commit_type: Some(ty),
        scope,
        breaking: breaking || breaking_body,
        description: description.trim(),
    }
}
pub fn extract_pr_numbers(text: &str) -> Vec<u32> {
    let mut out = Vec::new();
    for word in text.split_whitespace() {
        let value = word.trim_matches(|c: char| !c.is_ascii_digit() && c != '#');
        if let Some(n) = value.strip_prefix('#').and_then(|v| v.parse().ok()) {
            out.push(n);
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}
pub fn suggest_type<'a>(unknown: &str, known: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    fn distance(a: &str, b: &str) -> usize {
        let mut row: Vec<usize> = (0..=b.len()).collect();
        for (i, ca) in a.chars().enumerate() {
            let mut next = vec![i + 1];
            for (j, cb) in b.chars().enumerate() {
                next.push((row[j + 1] + 1).min(next[j] + 1).min(row[j] + usize::from(ca != cb)));
            }
            row = next;
        }
        row[b.len()]
    }
    known
        .into_iter()
        .map(|candidate| (candidate, distance(unknown, candidate)))
        .filter(|(_, d)| *d <= 2)
        .min_by_key(|(_, d)| *d)
        .map(|(candidate, _)| candidate)
}
