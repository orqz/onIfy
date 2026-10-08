//! Updates from the GitHub releases.
//!
//! How a new version gets installed depends on how this one was:
//! - a Flatpak: Flatpak updates it, so onify doesn't check at all;
//! - a distro package (pacman/yay, anything under /usr): the package manager
//!   updates it, so onify only says a new version is out;
//! - an AppImage: the file is swapped for the new one, then onify restarts;
//! - the Windows installer: the new installer runs silently and starts onify
//!   again when it's done;
//! - onify.app on a Mac: the new .dmg's app replaces this one, then reopens.
//!
//! Everything is fetched over HTTPS from github.com; the downloaded size must
//! match what the release lists.

use std::path::{Path, PathBuf};

use bytes::Bytes;
use http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use librespot_core::http_client::HttpClient;

const REPO: &str = "orqz/onify";
const CURRENT: &str = env!("ONIFY_VERSION");

pub struct Release {
    pub version: String,
    /// The release notes, as written on GitHub.
    pub notes: String,
    pub page: String,
    /// The file this install updates from (name, link, size), if any.
    asset: Option<(String, String, u64)>,
}

/// How this copy of onify was installed (see the module docs).
#[derive(Debug, Clone, PartialEq)]
pub enum Install {
    Flatpak,
    Package,
    /// The AppImage to replace.
    File(PathBuf),
    WindowsInstaller,
    MacApp(PathBuf),
    /// Built from source (install.sh), a development build, or somewhere
    /// onify can't update itself: it only says a new version is out.
    Manual,
}

impl Install {
    /// Whether onify can install the update itself.
    pub fn automatic(&self) -> bool {
        !matches!(self, Install::Flatpak | Install::Package | Install::Manual)
    }

    /// The release file this kind of install updates from.
    fn wants(&self, name: &str) -> bool {
        match self {
            Install::File(_) => name.ends_with(".AppImage") && name.contains(std::env::consts::ARCH),
            Install::WindowsInstaller => name.ends_with(".exe"),
            Install::MacApp(_) => name.ends_with(".dmg"),
            Install::Flatpak | Install::Package | Install::Manual => false,
        }
    }
}

impl Release {
    /// Whether this release has a file onify can install for `install`.
    pub fn installable(&self, install: &Install) -> bool {
        install.automatic() && self.asset.is_some()
    }
}

pub fn current() -> &'static str {
    CURRENT
}

pub fn install() -> Install {
    let Ok(exe) = std::env::current_exe() else { return Install::Manual };
    if exe.components().any(|c| c.as_os_str() == "target") {
        return Install::Manual;
    }
    if cfg!(target_os = "linux") {
        if Path::new("/.flatpak-info").exists() {
            return Install::Flatpak;
        }
        if let Some(appimage) = std::env::var_os("APPIMAGE") {
            return Install::File(appimage.into());
        }
        if exe.starts_with("/usr") || exe.starts_with("/opt") {
            return Install::Package;
        }
        return Install::Manual;
    }
    if cfg!(windows) {
        // bin\onify.exe next to the uninstaller Inno Setup leaves behind.
        let root = exe.parent().and_then(Path::parent);
        let installed = root.is_some_and(|r| r.join("unins000.exe").exists());
        return if installed { Install::WindowsInstaller } else { Install::Manual };
    }
    if cfg!(target_os = "macos") {
        // onify.app/Contents/MacOS/onify
        let bundle = exe.ancestors().nth(3).map(Path::to_path_buf);
        return match bundle {
            Some(b) if b.extension().is_some_and(|e| e == "app") && writable(b.parent()) => Install::MacApp(b),
            _ => Install::Manual,
        };
    }
    Install::Manual
}

fn writable(dir: Option<&Path>) -> bool {
    let Some(dir) = dir else { return false };
    let probe = dir.join(".onify-update-probe");
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(probe);
    ok
}

/// The latest release, if it's newer than this version.
pub async fn newer(install: &Install) -> Result<Option<Release>, String> {
    let api = std::env::var("ONIFY_UPDATE_API")
        .ok()
        .filter(|_| std::env::var_os("ONIFY_DEV").is_some())
        .unwrap_or_else(|| format!("https://api.github.com/repos/{REPO}/releases/latest"));
    let body = get(&api, Some("application/vnd.github+json")).await?;
    let v: serde_json::Value = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
    let version = v["tag_name"].as_str().unwrap_or_default().trim_start_matches('v').to_owned();
    if !is_newer(&version, CURRENT) {
        return Ok(None);
    }
    let asset = v["assets"].as_array().into_iter().flatten().find_map(|a| {
        let name = a["name"].as_str()?;
        install.wants(name).then(|| {
            let url = a["browser_download_url"].as_str()?.to_owned();
            Some((name.to_owned(), url, a["size"].as_u64().unwrap_or(0)))
        })?
    });
    Ok(Some(Release {
        version,
        notes: v["body"].as_str().unwrap_or_default().trim().to_owned(),
        page: v["html_url"].as_str().unwrap_or_default().to_owned(),
        asset,
    }))
}

/// "0.1.10" is newer than "0.1.9"; anything unparsable isn't newer.
fn is_newer(candidate: &str, current: &str) -> bool {
    let parse = |v: &str| -> Option<Vec<u64>> { v.split('.').map(|p| p.parse().ok()).collect() };
    match (parse(candidate), parse(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// GET, following GitHub's redirects to its download hosts.
async fn get(url: &str, accept: Option<&str>) -> Result<Bytes, String> {
    let client = HttpClient::new(None);
    let mut url = url.to_owned();
    for _ in 0..5 {
        let mut request = Request::get(&url);
        if let Some(accept) = accept {
            request = request.header(header::ACCEPT, accept);
        }
        let request = request.body(Bytes::new()).map_err(|e| e.to_string())?;
        let response = client.request_fut(request).map_err(|e| e.to_string())?.await.map_err(|e| e.to_string())?;
        let status = response.status();
        if status.is_redirection() {
            let next = response.headers().get(header::LOCATION).and_then(|l| l.to_str().ok());
            match next {
                Some(next) if next.starts_with("https://") => url = next.to_owned(),
                _ => return Err("bad redirect".into()),
            }
            continue;
        }
        if status == StatusCode::NOT_FOUND {
            return Err("no releases found".into());
        }
        if !status.is_success() {
            return Err(format!("GitHub said {status}"));
        }
        let body = response.into_body().collect().await.map_err(|e| e.to_string())?;
        return Ok(body.to_bytes());
    }
    Err("too many redirects".into())
}

/// Downloads the release and puts it in place. Afterwards the caller quits
/// onify; what's started here finishes the job and opens the new version.
pub async fn install_release(release: &Release, install: &Install) -> Result<(), String> {
    let Some((name, url, size)) = &release.asset else {
        return Err("this release has no download for this system".into());
    };
    let data = get(url, Some("application/octet-stream")).await?;
    if *size > 0 && data.len() as u64 != *size {
        return Err("the download was incomplete".into());
    }
    match install {
        Install::File(target) => replace_file(target, &data),
        Install::WindowsInstaller => run_installer(name, &data),
        Install::MacApp(bundle) => replace_app(bundle, &data),
        Install::Flatpak | Install::Package | Install::Manual => Err("onify can't update itself here".into()),
    }
}

/// Swaps the file (renaming over it is safe while it runs) and starts the
/// new one once this process has exited.
fn replace_file(target: &Path, data: &[u8]) -> Result<(), String> {
    let fresh = target.with_extension("update");
    std::fs::write(&fresh, data).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fresh, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&fresh, target).map_err(|e| e.to_string())?;
    relaunch_after_exit("exec \"$2\"", &[target.as_os_str()])
}

/// Deletes installers left in the temp folder by earlier updates (the one
/// that just ran may still be finishing; it goes on the next start).
pub fn clean_up_installers() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        // Installers from before the rename were named onIfy-setup-….
        let name = name.to_string_lossy().to_ascii_lowercase();
        if name.starts_with("onify-setup") && name.ends_with(".exe") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The installer replaces onify once it has quit, then starts it again (see
/// the [Run] section of packaging/windows/onify.iss).
fn run_installer(name: &str, data: &[u8]) -> Result<(), String> {
    let setup = std::env::temp_dir().join(name);
    std::fs::write(&setup, data).map_err(|e| e.to_string())?;
    std::process::Command::new(&setup)
        .args(["/SILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/CLOSEAPPLICATIONS"])
        .spawn()
        .map(drop)
        .map_err(|e| e.to_string())
}

/// Mounts the new .dmg once onify has quit, copies its app over this one
/// (keeping the old one until the copy worked) and opens it.
fn replace_app(bundle: &Path, data: &[u8]) -> Result<(), String> {
    let dmg = std::env::temp_dir().join("onify-update.dmg");
    std::fs::write(&dmg, data).map_err(|e| e.to_string())?;
    let script = r#"
        mnt=$(mktemp -d) || exit 1
        hdiutil attach -nobrowse -quiet -mountpoint "$mnt" "$2" || exit 1
        rm -rf "$3.old" && mv "$3" "$3.old" &&
            { ditto "$(ls -d "$mnt"/*.app | head -n 1)" "$3" && rm -rf "$3.old" || mv "$3.old" "$3"; }
        hdiutil detach -quiet "$mnt"
        rm -f "$2"
        open "$3"
    "#;
    relaunch_after_exit(script, &[dmg.as_os_str(), bundle.as_os_str()])
}

/// Runs `script` with sh once this process has exited; $2, $3… are `args`.
fn relaunch_after_exit(script: &str, args: &[&std::ffi::OsStr]) -> Result<(), String> {
    let wait = "while kill -0 \"$1\" 2>/dev/null; do sleep 0.2; done\n";
    std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{wait}{script}"))
        .arg("onify-update")
        .arg(std::process::id().to_string())
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(drop)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::is_newer;

    #[test]
    fn versions() {
        assert!(is_newer("0.1.1", "0.1.0"));
        assert!(is_newer("0.1.10", "0.1.9"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.0.9", "0.1.0"));
        assert!(!is_newer("garbage", "0.1.0"));
        assert!(is_newer("0.1.3.1", "0.1.3"));
        assert!(is_newer("0.1.4.0", "0.1.3.1"));
        assert!(!is_newer("0.1.3.1", "0.1.3.1"));
    }
}
