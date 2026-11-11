use crate::semver_util::{with_cached_range, with_cached_version};
use node_semver::{Identifier, Version};
use std::borrow::Cow;
use std::ops::Bound;
use std::rc::Rc;

/// pnpm 12's resolver verdict after declared-range normalization. A rejected
/// prerelease is retried as its base release to decide which importers need
/// inspection. This does not affect provider selection.
pub(crate) fn declared_peer_version_satisfies(version: &str, range: &str) -> bool {
    let normalized = peer_version_range(range);
    let range = &normalized;
    if range == "*" {
        return true;
    }
    with_cached_version(version, |parsed_version| {
        let (Some(version_parsed), Some(intervals)) = (parsed_version, peer_node_intervals(range))
        else {
            return version == normalized;
        };
        if intervals
            .iter()
            .any(|interval| interval.satisfies(version_parsed))
        {
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
        intervals.iter().any(|interval| interval.satisfies(&base))
    })
}

pub(crate) fn declared_peer_report_satisfies(version: &str, range: &str) -> bool {
    peer_version_satisfies(version, &peer_version_range(range))
}

// Mirrors get_peer_version_range in pnpm's resolving-resolver-base.
fn peer_version_range(range: &str) -> Cow<'_, str> {
    let valid = |range: &str| peer_node_intervals(range).is_some();
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
/// Try node-semver first, then tokenize prerelease intervals on whitespace,
/// including standalone operators, as pnpm's deps-inspection parser does.
pub fn peer_version_satisfies(version: &str, range: &str) -> bool {
    if range == "*" {
        return true;
    }
    if range.trim().is_empty() {
        return false;
    }
    with_cached_version(version, |parsed_version| {
        let (Some(version_parsed), Some(intervals)) = (parsed_version, peer_node_intervals(range))
        else {
            return version == range;
        };
        intervals
            .iter()
            .any(|interval| interval.satisfies(version_parsed))
            || (version_parsed.is_prerelease() && peer_prerelease_satisfies(version_parsed, range))
    })
}

// Parse individual primitives with the existing node-semver grammar, then fold
// them with pnpm/node-semver-rs 1ae976a's bound order. Keep this local to peer
// checks: dependency resolution must retain its existing range semantics.
#[derive(Clone)]
struct PeerInterval {
    lower: Bound<Version>,
    upper: Bound<Version>,
}

impl PeerInterval {
    fn intersect(&self, other: &Self) -> Option<Self> {
        let lower = match (&self.lower, &other.lower) {
            (Bound::Unbounded, _) => other.lower.clone(),
            (_, Bound::Unbounded) => self.lower.clone(),
            (left, right) => {
                let rank = |bound: &Bound<Version>| match bound {
                    Bound::Included(v) => (v.clone(), 0),
                    Bound::Excluded(v) => (v.clone(), 1),
                    Bound::Unbounded => unreachable!(),
                };
                if rank(left) >= rank(right) {
                    left.clone()
                } else {
                    right.clone()
                }
            }
        };
        let upper = match (&self.upper, &other.upper) {
            (Bound::Unbounded, _) => other.upper.clone(),
            (_, Bound::Unbounded) => self.upper.clone(),
            (left, right) => {
                let rank = |bound: &Bound<Version>| match bound {
                    Bound::Excluded(v) => (v.clone(), 0),
                    Bound::Included(v) => (v.clone(), 1),
                    Bound::Unbounded => unreachable!(),
                };
                if rank(left) <= rank(right) {
                    left.clone()
                } else {
                    right.clone()
                }
            }
        };
        let valid = match (&lower, &upper) {
            (Bound::Unbounded, _) | (_, Bound::Unbounded) => true,
            (Bound::Included(a), Bound::Included(b)) => a <= b,
            (Bound::Included(a) | Bound::Excluded(a), Bound::Included(b) | Bound::Excluded(b)) => {
                a < b
            }
        };
        valid.then_some(Self { lower, upper })
    }

    fn satisfies(&self, version: &Version) -> bool {
        let above = match &self.lower {
            Bound::Included(lower) => version >= lower,
            Bound::Excluded(lower) => version > lower,
            Bound::Unbounded => true,
        };
        let below = match &self.upper {
            Bound::Included(upper) => version <= upper,
            Bound::Excluded(upper) => version < upper,
            Bound::Unbounded => true,
        };
        above
            && below
            && (!version.is_prerelease()
                || [&self.lower, &self.upper].iter().any(|bound| match bound {
                    Bound::Included(v) | Bound::Excluded(v) => {
                        v.is_prerelease()
                            && (v.major, v.minor, v.patch)
                                == (version.major, version.minor, version.patch)
                    }
                    Bound::Unbounded => false,
                }))
    }
}

fn peer_node_intervals(range: &str) -> Option<Rc<Vec<PeerInterval>>> {
    thread_local! {
        static CACHE: std::cell::RefCell<crate::FxHashMap<String, Option<Rc<Vec<PeerInterval>>>>> =
            std::cell::RefCell::default();
    }
    CACHE.with(|cell| {
        let mut cache = cell.borrow_mut();
        if let Some(parsed) = cache.get(range) {
            return parsed.clone();
        }
        let parsed = parse_peer_node_intervals(range).map(Rc::new);
        cache.insert(range.to_string(), parsed.clone());
        parsed
    })
}

fn parse_peer_node_intervals(range: &str) -> Option<Vec<PeerInterval>> {
    let mut intervals = Vec::new();
    for (alternative_index, alternative) in range.split("||").enumerate() {
        let mut folded: Vec<PeerInterval> = Vec::new();
        let tokens: Vec<_> = alternative
            .split([' ', '\t'])
            .filter(|token| !token.is_empty())
            .collect();
        let mut index = 0;
        while index < tokens.len() {
            let mut primitive = tokens[index].to_string();
            index += 1;
            if [">=", ">", "<=", "<", "=", "~", "^", "~>"].contains(&primitive.as_str())
                && index < tokens.len()
            {
                primitive.push(' ');
                primitive.push_str(tokens[index]);
                index += 1;
            } else if index + 1 < tokens.len()
                && tokens[index] == "-"
                && primitive
                    .starts_with(|c: char| c.is_ascii_digit() || matches!(c, 'v' | 'x' | 'X' | '*'))
            {
                primitive = format!("{primitive} - {}", tokens[index + 1]);
                index += 2;
            }
            if primitive == "-"
                && alternative_index == 0
                && index == 1
                && alternative.starts_with([' ', '\t'])
                && index < tokens.len()
            {
                primitive = format!(" - {}", tokens[index]);
                index += 1;
            }
            if !with_cached_range(&primitive, |parsed| parsed.is_some()) {
                continue;
            }
            // 7aa47b1 expands <= partials before the unchanged primitive parser.
            if let Some(raw) = primitive.strip_prefix("<=") {
                let raw = raw
                    .trim_start()
                    .strip_prefix('v')
                    .unwrap_or(raw.trim_start());
                let components: Vec<_> = raw
                    .split(['-', '+'])
                    .next()
                    .unwrap_or(raw)
                    .split('.')
                    .collect();
                let wildcard = |part: &str| matches!(part, "*" | "x" | "X");
                if wildcard(components[0]) {
                    primitive = ">=0.0.0".to_string();
                } else if components.len() < 3 || wildcard(components[2]) {
                    let major = components[0].parse::<u64>().ok();
                    let minor = components
                        .get(1)
                        .filter(|part| !wildcard(part))
                        .and_then(|part| part.parse::<u64>().ok());
                    if let Some(major) = major {
                        let upper = match minor {
                            Some(minor) => prerelease_bound(major, minor.checked_add(1)?, 0),
                            None => prerelease_bound(major.checked_add(1)?, 0, 0),
                        };
                        primitive = format!("<{upper}");
                    }
                }
            }
            let rendered = with_cached_range(&primitive, |parsed| parsed.map(ToString::to_string));
            let Some(rendered) = rendered else { continue };
            let mut interval = PeerInterval {
                lower: Bound::Unbounded,
                upper: Bound::Unbounded,
            };
            for comparator in rendered.split_whitespace() {
                let (lower, upper) = peer_comparator(comparator)?;
                interval = interval.intersect(&PeerInterval { lower, upper })?;
            }
            if let Some(last) = folded.pop() {
                if let Some(intersection) = last.intersect(&interval) {
                    folded.push(intersection);
                } else {
                    // node-semver's parser retains disjoint primitives as
                    // alternatives instead of turning the range into empty.
                    folded.push(last);
                    folded.push(interval);
                }
            } else {
                folded.push(interval);
            }
        }
        intervals.extend(folded);
    }
    (!intervals.is_empty()).then_some(intervals)
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
            part.split_whitespace().map(peer_comparator).collect()
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
        .map(|part| part.replace(['x', 'X', '*'], "0"))
        .collect();
    let normalized = if numeric
        .iter()
        .all(|part| part.chars().all(|c| c.is_ascii_digit()))
    {
        numeric.resize(3, "0".to_string());
        numeric.extend(parts.iter().skip(3).map(|part| part.to_string()));
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
    fn interval_normalization_replaces_embedded_wildcards() {
        let version = Version::parse("10.5.0-beta").unwrap();
        for range in [">=1x", ">=1X", ">=1*"] {
            assert!(peer_prerelease_satisfies(&version, range), "{range}");
        }
    }

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
            ("1.0.0", "3 || - 2", false),
            ("2.0.0", "3 || - 2", true),
            ("1.0.0", " - 2", true),
            ("1.2.9", "<=1.2", true),
            ("1.9.0", "<=1", true),
            ("1.3.0", "<=1.2", false),
            ("1.3.0-beta", "<=1.2", false),
            ("1.0.0-beta", "<1", false),
            ("1.5.0-beta", "1.0.0 - 1.9.0", true),
            ("custom-tag", "custom-tag", true),
            ("custom-tag", "*", true),
            ("19.0.0", "not-a-range", false),
            ("19.0.0", "<=*", true),
            ("19.0.0", "<=x", true),
            ("19.0.0", "<= *", true),
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
