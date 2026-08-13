use std::{fs, io, os::unix::fs::PermissionsExt, path::Path, process::Command};

use svarm_agent::{Result, paths::RuntimePaths};

use crate::client::{ControlClient, Probe};

const RELEASE_API_URL: &str = "https://api.github.com/repos/svarmdev/svarm/releases/latest";
const RELEASE_DOWNLOAD_URL: &str = "https://github.com/svarmdev/svarm/releases/download";

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ReleaseVersion {
    major: u64,
    minor: u64,
    patch: u64,
}

impl std::fmt::Display for ReleaseVersion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Release {
    tag: String,
    version: ReleaseVersion,
}

pub(crate) fn run(paths: &RuntimePaths, yes: bool) -> Result<()> {
    let executable = std::env::current_exe()?;
    let install_dir = executable
        .parent()
        .ok_or("the running Svarm executable has no installation directory")?;
    if fs::symlink_metadata(&executable)?.file_type().is_symlink() {
        return Err(format!(
            "cannot upgrade a symbolic-link executable; install Svarm directly in {}",
            install_dir.display()
        )
        .into());
    }
    let temporary_directory = std::env::temp_dir().join(format!(
        "svarm-upgrade-{}-{}",
        std::process::id(),
        crate::unix_time_ms()
    ));
    fs::create_dir(&temporary_directory).map_err(|error| {
        format!(
            "could not create temporary upgrade directory {}: {error}",
            temporary_directory.display()
        )
    })?;

    let result = (|| {
        let release_json = temporary_directory.join("release.json");
        download_file(RELEASE_API_URL, &release_json)?;
        let release = parse_latest_release(&fs::read_to_string(&release_json)?)?;
        let current = parse_version(env!("CARGO_PKG_VERSION"))
            .expect("CARGO_PKG_VERSION must be a three-part release version");
        if release.version <= current {
            println!(
                "Svarm {} is already up to date (latest: {}).",
                current, release.version
            );
            return Ok(());
        }

        let target = release_target()?;
        let archive_name = format!("svarm-{}-{target}.tar.gz", release.version);
        let archive = temporary_directory.join(&archive_name);
        let checksums = temporary_directory.join("SHA256SUMS");
        let release_url = format!("{RELEASE_DOWNLOAD_URL}/{}/{archive_name}", release.tag);
        let checksum_url = format!("{RELEASE_DOWNLOAD_URL}/{}/SHA256SUMS", release.tag);
        download_file(&release_url, &archive)?;
        download_file(&checksum_url, &checksums)?;
        verify_checksum(&archive, &archive_name, &fs::read_to_string(&checksums)?)?;

        let extracted = temporary_directory.join("extracted");
        fs::create_dir(&extracted)?;
        verify_archive_layout(&archive)?;
        extract_archive(&archive, &extracted)?;
        let new_binary = extracted.join("svarm");
        if !new_binary.is_file() {
            return Err("release archive does not contain a svarm binary".into());
        }

        let server_running = !matches!(ControlClient::probe_socket(&paths.socket)?, Probe::None);
        if server_running {
            eprintln!(
                "WARNING: the Svarm server is running. Upgrading will stop the server and close all agents."
            );
            if !yes && !crate::confirm("Continue with the upgrade")? {
                println!("Upgrade cancelled.");
                return Ok(());
            }
        }

        let replacement = install_dir.join(format!(".svarm-upgrade-{}.tmp", std::process::id()));
        fs::copy(&new_binary, &replacement).map_err(|error| {
            format!(
                "could not prepare the replacement in {}: {error}",
                install_dir.display()
            )
        })?;
        fs::set_permissions(&replacement, fs::Permissions::from_mode(0o755))?;

        let result = (|| {
            if server_running {
                crate::stop_server(paths, true)?;
            }
            fs::rename(&replacement, &executable)
                .map_err(|error| format!("could not replace {}: {error}", executable.display()))?;
            println!("Upgraded Svarm {} -> {}.", current, release.version);
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&replacement);
        }
        result
    })();
    let _ = fs::remove_dir_all(&temporary_directory);
    result
}

fn download_file(url: &str, destination: &Path) -> Result<()> {
    match Command::new("curl")
        .args(["-fL", "--retry", "3", "-A", "svarm", "-o"])
        .arg(destination)
        .arg(url)
        .status()
    {
        Ok(status) if status.success() => return Ok(()),
        Ok(status) => return Err(format!("curl could not download {url} ({status})").into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("could not run curl: {error}").into()),
    }

    match Command::new("wget")
        .args(["-q", "--tries=3", "--user-agent=svarm", "-O"])
        .arg(destination)
        .arg(url)
        .status()
    {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!("wget could not download {url} ({status})").into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Err("svarm upgrade requires curl or wget to download releases".into())
        }
        Err(error) => Err(format!("could not run wget: {error}").into()),
    }
}

fn parse_latest_release(json: &str) -> Result<Release> {
    let value: serde_json::Value = serde_json::from_str(json)
        .map_err(|error| format!("GitHub returned invalid release metadata: {error}"))?;
    let tag = value
        .get("tag_name")
        .and_then(serde_json::Value::as_str)
        .ok_or("GitHub release metadata did not contain tag_name")?;
    let version = parse_release_tag(tag)
        .ok_or("GitHub latest release tag is not a vMAJOR.MINOR.PATCH version")?;
    Ok(Release {
        tag: tag.to_owned(),
        version,
    })
}

fn parse_release_tag(tag: &str) -> Option<ReleaseVersion> {
    tag.strip_prefix('v').and_then(parse_version)
}

fn parse_version(value: &str) -> Option<ReleaseVersion> {
    let mut parts = value.split('.');
    let version = ReleaseVersion {
        major: parts.next()?.parse().ok()?,
        minor: parts.next()?.parse().ok()?,
        patch: parts.next()?.parse().ok()?,
    };
    parts.next().is_none().then_some(version)
}

fn release_target() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-gnu"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        (os, arch) => Err(format!(
            "unsupported platform: {os}/{arch} (supported: Linux x86_64/ARM64 and macOS Intel/Apple Silicon)"
        )
        .into()),
    }
}

fn verify_checksum(archive: &Path, archive_name: &str, checksums: &str) -> Result<()> {
    let expected = expected_checksum(checksums, archive_name)?;
    let actual = sha256_file(archive)?;
    if actual.eq_ignore_ascii_case(&expected) {
        Ok(())
    } else {
        Err(format!("checksum verification failed for {archive_name}").into())
    }
}

fn expected_checksum(checksums: &str, archive_name: &str) -> Result<String> {
    checksums
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let checksum = fields.next()?;
            let file = fields.next()?;
            let file = file.strip_prefix('*').unwrap_or(file);
            (file == archive_name
                && checksum.len() == 64
                && checksum.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .then_some(checksum.to_ascii_lowercase())
        })
        .next()
        .ok_or_else(|| format!("SHA256SUMS does not contain a checksum for {archive_name}"))
        .map_err(Into::into)
}

fn sha256_file(path: &Path) -> Result<String> {
    for (program, arguments) in [("sha256sum", Vec::new()), ("shasum", vec!["-a", "256"])] {
        let output = match Command::new(program).args(arguments).arg(path).output() {
            Ok(output) => output,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("could not run {program}: {error}").into()),
        };
        if !output.status.success() {
            return Err(format!("{program} could not hash {}", path.display()).into());
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let checksum = stdout
            .split_whitespace()
            .next()
            .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .ok_or_else(|| format!("{program} returned an invalid SHA-256 checksum"))?;
        return Ok(checksum.to_ascii_lowercase());
    }
    Err("svarm upgrade requires sha256sum or shasum for checksum verification".into())
}

fn verify_archive_layout(archive: &Path) -> Result<()> {
    let output = Command::new("tar")
        .args(["-tzf"])
        .arg(archive)
        .output()
        .map_err(|error| format!("could not run tar: {error}"))?;
    if !output.status.success() {
        return Err(format!("could not inspect release archive {}", archive.display()).into());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let entries = stdout
        .lines()
        .filter(|entry| !entry.is_empty())
        .collect::<Vec<_>>();
    if entries.len() == 1 && entries[0] == "svarm" {
        Ok(())
    } else {
        Err("release archive contains unexpected files".into())
    }
}

fn extract_archive(archive: &Path, destination: &Path) -> Result<()> {
    let status = Command::new("tar")
        .args(["-xzf"])
        .arg(archive)
        .args(["-C"])
        .arg(destination)
        .status()
        .map_err(|error| format!("could not run tar: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("could not extract release archive ({status})").into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_metadata_requires_a_v_three_part_tag() {
        assert_eq!(
            parse_latest_release(r#"{"tag_name":"v1.2.3"}"#)
                .unwrap()
                .version,
            ReleaseVersion {
                major: 1,
                minor: 2,
                patch: 3,
            }
        );
        assert!(parse_latest_release(r#"{"tag_name":"release-1.2.3"}"#).is_err());
        assert!(parse_latest_release(r#"{"name":"v1.2.3"}"#).is_err());
    }

    #[test]
    fn release_versions_compare_numerically() {
        assert!(parse_version("1.10.0") > parse_version("1.9.9"));
        assert!(parse_version("1.2.3") <= parse_version("1.2.3"));
    }

    #[test]
    fn checksum_manifest_matches_normal_and_binary_filenames() {
        let checksum = "aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899";
        let manifest = format!("{checksum}  svarm.tar.gz\n");
        assert_eq!(
            expected_checksum(&manifest, "svarm.tar.gz").unwrap(),
            checksum
        );

        let manifest = format!("{checksum} *svarm.tar.gz\n");
        assert_eq!(
            expected_checksum(&manifest, "svarm.tar.gz").unwrap(),
            checksum
        );
        assert!(expected_checksum(&manifest, "other.tar.gz").is_err());
    }

    #[test]
    fn checksum_verification_matches_known_content() {
        let path = std::env::temp_dir().join(format!("svarm-checksum-test-{}", std::process::id()));
        fs::write(&path, b"hello").unwrap();
        let manifest = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824  archive";
        assert!(verify_checksum(&path, "archive", manifest).is_ok());
        fs::remove_file(path).unwrap();
    }
}
