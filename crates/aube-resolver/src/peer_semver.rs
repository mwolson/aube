use crate::semver_util::{with_cached_range, with_cached_version};
use node_semver::{Identifier, Version};
use std::borrow::Cow;
use std::ops::Bound;

/// Accept a declared peer when either pnpm 12's resolver check or its
/// inspection interval check accepts it, after declared-range normalization.
/// The resolver retries prereleases as their base release. Provider selection
/// is unchanged; pnpm's per-importer report can list additional mismatches.
pub(crate) fn declared_peer_version_satisfies(version: &str, range: &str) -> bool {
    let normalized = peer_version_range(range);
    let range = expand_partial_lte(&normalized);
    if range == "*" {
        return true;
    }
    with_cached_version(version, |parsed_version| {
        with_cached_range(&range, |parsed_range| {
            let (Some(version_parsed), Some(range_parsed)) = (parsed_version, parsed_range) else {
                return version == normalized;
            };
            if range_parsed.satisfies(version_parsed) {
                return true;
            }
            if !version_parsed.is_prerelease() {
                return false;
            }
            let base = Version {
                major: version_parsed.major,
                minor: version_parsed.minor,
                patch: version_parsed.patch,
                pre_release: Vec::new(),
                build: Vec::new(),
            };
            range_parsed.satisfies(&base)
        })
    }) || peer_version_satisfies(version, &normalized)
}

// Mirrors get_peer_version_range in pnpm's resolving-resolver-base.
fn peer_version_range(range: &str) -> Cow<'_, str> {
    let valid = |range: &str| with_cached_range(range, |parsed| parsed.is_some());
    if range.contains("||") {
        return Cow::Owned(
            range
                .split("||")
                .map(|part| peer_version_range(part.trim()))
                .collect::<Vec<_>>()
                .join(" || "),
        );
    }
    if valid(range) || range.contains("workspace:") || range.contains("catalog:") {
        return Cow::Borrowed(match range.strip_prefix("workspace:") {
            Some(body) if valid(body) => body,
            Some(_) => "*",
            None => range,
        });
    }
    if let Some((scheme, body)) = range.split_once(':')
        && !scheme.is_empty()
    {
        if valid(body) {
            return Cow::Borrowed(body);
        }
        if let Some((name, body)) = body.rsplit_once('@')
            && !name.is_empty()
            && valid(body)
        {
            return Cow::Borrowed(body);
        }
    }
    Cow::Borrowed("*")
}

/// Check allowedVersions values and parent ranges using pnpm's deps-inspection
/// interval bounds. Empty rules match nothing; `*` admits any version.
/// Deliberately join spaced operators to their versions so `>= 4.0.0` has its
/// usual meaning, unlike pnpm's deps-inspection parser. Provider selection is
/// unchanged; declared-peer detection also uses this check after normalization.
pub fn peer_version_satisfies(version: &str, range: &str) -> bool {
    if range == "*" {
        return true;
    }
    if range.trim().is_empty() {
        return false;
    }
    let comparable = expand_partial_lte(range);
    with_cached_version(version, |parsed_version| {
        with_cached_range(&comparable, |parsed_range| {
            let (Some(version_parsed), Some(range_parsed)) = (parsed_version, parsed_range) else {
                return version == range;
            };
            range_parsed.satisfies(version_parsed)
                || (version_parsed.is_prerelease()
                    && peer_prerelease_satisfies(version_parsed, range))
        })
    })
}

// pnpm patches node-semver's partial <= bounds; keep that correction local
// to peer checks instead of changing ordinary dependency selection.
fn expand_partial_lte(range: &str) -> Cow<'_, str> {
    if !range.contains("<=") {
        return Cow::Borrowed(range);
    }
    let separated = range.replace("||", " || ");
    let mut tokens = separated.split_whitespace();
    let mut comparators = Vec::new();
    while let Some(token) = tokens.next() {
        let comparator = if token == "<=" {
            match tokens.next() {
                Some(version) => format!("<={version}"),
                None => return Cow::Borrowed(range),
            }
        } else {
            token.to_string()
        };
        let expanded = comparator.strip_prefix("<=").and_then(|raw| {
            let raw = raw.strip_prefix('v').unwrap_or(raw);
            match peer_comparator(&format!("<={raw}"))? {
                (_, Bound::Excluded(upper)) => Some(format!("<{upper}")),
                _ => None,
            }
        });
        comparators.push(expanded.unwrap_or(comparator));
    }
    Cow::Owned(comparators.join(" "))
}

fn peer_prerelease_satisfies(version_parsed: &Version, range: &str) -> bool {
    // pnpm's deps-inspection-peers checks plain interval bounds for
    // prereleases, retaining -0 on derived upper bounds. Its separate
    // semver-include-prerelease crate also lowers partial lower bounds;
    // peer-rule filtering does not do that.
    let alternatives: Option<Vec<Vec<_>>> = range
        .split("||")
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| {
            let part = if let Some((start, end)) = part.split_once(" - ") {
                format!(">={} <={}", start.trim(), end.trim())
            } else {
                part.to_string()
            };
            let mut comparators = Vec::new();
            let mut tokens = part.split_whitespace();
            while let Some(token) = tokens.next() {
                let comparator = if matches!(token, ">=" | ">" | "<=" | "<" | "^" | "~" | "=") {
                    format!("{token}{}", tokens.next()?)
                } else {
                    token.to_string()
                };
                comparators.push(peer_comparator(&comparator)?);
            }
            Some(comparators)
        })
        .collect();
    alternatives.is_some_and(|alternatives| {
        alternatives.iter().any(|comparators| {
            comparators.iter().all(|(lower, upper)| {
                let above = match lower {
                    Bound::Included(lower) => version_parsed >= lower,
                    Bound::Excluded(lower) => version_parsed > lower,
                    Bound::Unbounded => true,
                };
                let below = match upper {
                    Bound::Included(upper) => version_parsed <= upper,
                    Bound::Excluded(upper) => version_parsed < upper,
                    Bound::Unbounded => true,
                };
                above && below
            })
        })
    })
}

fn peer_comparator(comparator: &str) -> Option<(Bound<Version>, Bound<Version>)> {
    use Bound::{Excluded, Included, Unbounded};

    let (operator, raw) = [">=", ">", "<=", "<", "^", "~"]
        .into_iter()
        .find_map(|operator| comparator.strip_prefix(operator).map(|raw| (operator, raw)))
        .unwrap_or(("=", comparator));
    let specificity = raw
        .split('.')
        .take(3)
        .take_while(|part| {
            let head = part.split(['-', '+']).next().unwrap_or(part);
            !head.is_empty() && !head.chars().all(|c| matches!(c, 'x' | 'X' | '*'))
        })
        .count();
    if specificity == 0 {
        return Some((Unbounded, Unbounded));
    }
    let parts: Vec<_> = raw.split('.').collect();
    let mut numeric: Vec<_> = parts
        .iter()
        .take(3)
        .map(|part| {
            if matches!(*part, "x" | "X" | "*") {
                "0"
            } else {
                *part
            }
        })
        .collect();
    let normalized = if numeric
        .iter()
        .all(|part| part.chars().all(|c| c.is_ascii_digit()))
    {
        numeric.resize(3, "0");
        numeric.extend(parts.iter().skip(3).copied());
        numeric.join(".")
    } else {
        raw.to_string()
    };
    let version = Version::parse(normalized).ok()?;
    let next_unpinned = || {
        if specificity >= 2 {
            Some(prerelease_bound(
                version.major,
                version.minor.checked_add(1)?,
                0,
            ))
        } else {
            Some(prerelease_bound(version.major.checked_add(1)?, 0, 0))
        }
    };
    Some(match operator {
        "=" if specificity == 3 => (Included(version.clone()), Included(version)),
        "=" | "~" => (Included(version.clone()), Excluded(next_unpinned()?)),
        ">=" => (Included(version), Unbounded),
        ">" if specificity == 3 => (Excluded(version), Unbounded),
        ">" => (Included(next_unpinned()?), Unbounded),
        "<=" if specificity == 3 => (Unbounded, Included(version)),
        "<=" => (Unbounded, Excluded(next_unpinned()?)),
        "<" if specificity == 3 => (Unbounded, Excluded(version)),
        "<" => (
            Unbounded,
            Excluded(prerelease_bound(
                version.major,
                version.minor,
                version.patch,
            )),
        ),
        "^" => {
            let upper = if specificity == 1 || version.major > 0 {
                prerelease_bound(version.major.checked_add(1)?, 0, 0)
            } else if specificity == 2 || version.minor > 0 {
                prerelease_bound(0, version.minor.checked_add(1)?, 0)
            } else {
                prerelease_bound(0, 0, version.patch.checked_add(1)?)
            };
            (Included(version), Excluded(upper))
        }
        _ => return None,
    })
}

fn prerelease_bound(major: u64, minor: u64, patch: u64) -> Version {
    Version {
        major,
        minor,
        patch,
        pre_release: vec![Identifier::Numeric(0)],
        build: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_ranges_keep_deps_inspection_prerelease_bounds() {
        for (version, range, satisfied) in [
            ("19.3.0-canary-x", "*", true),
            ("19.3.0-canary-x", "^19.0.0", true),
            ("19.0.0-rc.1", "^19.0.0", false),
            ("20.0.0-beta.1", "^19.0.0", false),
            ("20.0.0-beta.1", "<20.0.0", true),
            ("1.9.5-beta", "~1.9.0", true),
            ("2.0.0-beta", "~1.9.0", false),
            ("1.0.0-beta", "1", false),
            ("1.2.9-beta", "<=1.2", true),
            ("1.2.9", "<=1.2", true),
            ("1.9.0", "<=1", true),
            ("1.3.0", "<=1.2", false),
            ("1.3.0-beta", "<=1.2", false),
            ("1.0.0-beta", "<1", false),
            ("1.5.0-beta", "1.0.0 - 1.9.0", true),
            ("custom-tag", "custom-tag", true),
            ("custom-tag", "*", true),
            ("19.0.0", "not-a-range", false),
            ("19.0.0", "", false),
        ] {
            assert_eq!(
                peer_version_satisfies(version, range),
                satisfied,
                "{version} against {range}"
            );
        }
    }
}
