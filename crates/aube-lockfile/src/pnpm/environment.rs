use crate::Error;
use aube_manifest::PackageJson;
use std::{io::ErrorKind, path::Path};

pub(super) fn read_for_write(path: &Path, manifest: &PackageJson) -> Result<Option<String>, Error> {
    let is_pnpm_lockfile = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("pnpm-lock.") && name.ends_with(".yaml"));
    if !is_pnpm_lockfile {
        return Ok(None);
    }
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(Error::Io(path.to_path_buf(), error)),
    };
    let Some((document, prefix)) = split_environment(&content) else {
        return Ok(None);
    };
    let environment: yaml_serde::Value =
        yaml_serde::from_str(document).map_err(|error| Error::parse(path, error.to_string()))?;
    let importer = &environment["importers"]["."];
    if importer.get("configDependencies").is_none()
        && importer.get("packageManagerDependencies").is_none()
    {
        return Ok(None);
    }
    // pnpm owns this graph, including platform-specific bootstrap packages.
    // Keep it even without a pin: config dependencies may still need it.
    if let Some(wanted) = wanted_pnpm_version(manifest)
        && let Some(dependencies) = importer["packageManagerDependencies"].as_mapping()
    {
        for dependency in dependencies.values() {
            let version = dependency["version"].as_str().unwrap_or("");
            let satisfies = version == wanted
                || node_semver::Version::parse(version).is_ok_and(|version| {
                    node_semver::Range::parse(wanted).is_ok_and(|range| range.satisfies(&version))
                });
            if !satisfies {
                return Err(Error::parse(
                    path,
                    format!(
                        "cannot preserve packageManagerDependencies for pnpm {version:?} with \
                         the requested pnpm {wanted:?}; run `pnpm install --no-frozen-lockfile` \
                         to refresh pnpm's environment document before rewriting with aube"
                    ),
                ));
            }
        }
    }
    Ok(Some(prefix))
}

// pnpm marks its environment document with an opening `---`. Keep the
// original bytes, including comments, BOM, CRLF and the next document marker.
fn split_environment(content: &str) -> Option<(&str, String)> {
    let text = content.strip_prefix('\u{feff}').unwrap_or(content);
    let start = content.len() - text.len();
    let mut lines = text.split_inclusive('\n');
    let opening = lines.next()?;
    if opening.trim_end_matches(['\r', '\n']) != "---" {
        return None;
    }
    let body_start = start + opening.len();
    let mut offset = body_start;
    for line in lines {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            let mut prefix = content[..offset + line.len()].to_string();
            if !prefix.ends_with('\n') {
                prefix.push('\n');
            }
            return Some((&content[body_start..offset], prefix));
        }
        offset += line.len();
    }
    let mut prefix = content.to_string();
    if !prefix.ends_with('\n') {
        prefix.push('\n');
    }
    prefix.push_str("---\n");
    Some((&content[body_start..], prefix))
}

fn wanted_pnpm_version(manifest: &PackageJson) -> Option<&str> {
    let engines = manifest
        .dev_engines
        .as_ref()
        .map(|engines| &engines.package_manager);
    if let Some(engine) = engines.and_then(|engines| {
        engines
            .iter()
            .find(|engine| engine.name == "pnpm")
            .or_else(|| engines.first())
    }) {
        return (engine.name == "pnpm")
            .then_some(engine.version.as_deref())
            .flatten();
    }
    let reference = manifest
        .extra
        .get("packageManager")?
        .as_str()?
        .strip_prefix("pnpm@")?;
    let version = reference.split('+').next()?;
    let parsed = node_semver::Version::parse(version).ok()?;
    (parsed.to_string() == version).then_some(version)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LockfileGraph, pnpm::write_with_project_root};

    const ENVIRONMENT: &str = "---\n# pnpm owns this document\nlockfileVersion: '9.0'\nimporters:\n  .:\n    configDependencies: {}\n    packageManagerDependencies:\n      pnpm:\n        specifier: 12.14.0\n        version: 12.14.0\npackages: {}\nsnapshots: {}\n";
    const PROJECT: &str = "lockfileVersion: '9.0'\nsettings:\n  autoInstallPeers: true\nimporters:\n  .: {}\npackages: {}\nsnapshots: {}\n";

    #[test]
    fn rewrite_preserves_environment_with_matching_or_removed_pin() {
        for name in ["pnpm-lock.yaml", "pnpm-lock.feature.yaml"] {
            for pin in [Some("pnpm@12.14.0"), Some("pnpm@12.14.0+sha512.abc"), None] {
                let directory = tempfile::tempdir().unwrap();
                let path = directory.path().join(name);
                let prefix = format!("{ENVIRONMENT}\n---\n");
                std::fs::write(&path, format!("{prefix}{PROJECT}")).unwrap();
                let manifest: PackageJson = serde_json::from_value(serde_json::json!({
                    "packageManager": pin,
                }))
                .unwrap();
                let mut graph = crate::pnpm::parse(&path).unwrap();
                graph.settings.auto_install_peers = false;
                write_with_project_root(&path, directory.path(), &graph, &manifest).unwrap();
                let output = std::fs::read_to_string(&path).unwrap();
                assert!(output.starts_with(&prefix));
                assert!(
                    output.contains("autoInstallPeers: false"),
                    "project document was rewritten"
                );
                assert!(crate::pnpm::parse(&path).unwrap().packages.is_empty());
            }
        }
    }

    #[test]
    fn rewrite_refuses_stale_environment_without_changing_lockfile() {
        for policy in [
            serde_json::json!({"packageManager": "pnpm@12.13.0"}),
            serde_json::json!({"devEngines": {"packageManager": {"name": "pnpm", "version": "^13"}}}),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("pnpm-lock.yaml");
            let original = format!("{ENVIRONMENT}\n---\n{PROJECT}");
            std::fs::write(&path, &original).unwrap();
            let manifest = serde_json::from_value(policy).unwrap();
            let error = write_with_project_root(
                &path,
                directory.path(),
                &LockfileGraph::default(),
                &manifest,
            )
            .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("run `pnpm install --no-frozen-lockfile`")
            );
            assert_eq!(std::fs::read_to_string(path).unwrap(), original);
        }
    }

    #[test]
    fn dev_engines_pin_takes_precedence_over_legacy_pin() {
        let manifest = serde_json::from_value(serde_json::json!({
            "packageManager": "pnpm@13.0.0",
            "devEngines": {"packageManager": {"name": "pnpm", "version": "^12.0.0"}},
        }))
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("pnpm-lock.yaml");
        std::fs::write(&path, format!("{ENVIRONMENT}\n---\n{PROJECT}")).unwrap();
        assert!(read_for_write(&path, &manifest).unwrap().is_some());
    }

    #[test]
    fn environment_only_lockfile_retains_its_document() {
        for ending in ["", "---", "---\n"] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("pnpm-lock.yaml");
            std::fs::write(&path, format!("{ENVIRONMENT}{ending}")).unwrap();
            write_with_project_root(
                &path,
                directory.path(),
                &LockfileGraph::default(),
                &PackageJson::default(),
            )
            .unwrap();
            let output = std::fs::read_to_string(&path).unwrap();
            assert!(output.starts_with(&format!("{ENVIRONMENT}---\n")));
            assert!(crate::pnpm::parse(&path).is_ok());
        }
    }

    #[test]
    fn single_project_document_and_aube_lockfile_do_not_gain_environment() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("pnpm-lock.yaml");
        std::fs::write(&path, format!("---\n{PROJECT}")).unwrap();
        assert!(
            read_for_write(&path, &PackageJson::default())
                .unwrap()
                .is_none()
        );
        let aube_path = directory.path().join("aube-lock.yaml");
        std::fs::write(&aube_path, format!("{ENVIRONMENT}\n---\n{PROJECT}")).unwrap();
        assert!(
            read_for_write(&aube_path, &PackageJson::default())
                .unwrap()
                .is_none()
        );
    }

    proptest::proptest! {
        #[test]
        fn environment_prefix_survives_rewrites_verbatim(
            comment in "[^\\p{C}\\p{Zl}\\p{Zp}]{0,60}",
            crlf in proptest::bool::ANY,
            bom in proptest::bool::ANY,
        ) {
            let mut prefix = format!("{ENVIRONMENT}# {comment}\n\n---\n");
            if crlf { prefix = prefix.replace('\n', "\r\n"); }
            if bom { prefix.insert(0, '\u{feff}'); }
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("pnpm-lock.yaml");
            std::fs::write(&path, format!("{prefix}{PROJECT}")).unwrap();
            write_with_project_root(&path, directory.path(), &LockfileGraph::default(), &PackageJson::default()).unwrap();
            proptest::prop_assert!(std::fs::read_to_string(path).unwrap().starts_with(&prefix));
        }
    }
}
