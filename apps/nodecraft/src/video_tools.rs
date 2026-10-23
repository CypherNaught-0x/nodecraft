//! Resolve installed tools once at native startup, before the UI frame loop. Nothing from a
//! project participates and no executable is started here; the media worker checks capabilities.
use nodecraft_engine::settings::MediaSettings;
use std::ffi::OsStr;

const MAX_PATH_BYTES: usize = 64 * 1024;
const MAX_DIRECTORIES: usize = 256;

pub fn discover(settings: &MediaSettings) -> Result<MediaSettings, &'static str> {
    resolve(
        settings,
        std::env::var_os("NODECRAFT_FFMPEG").as_deref(),
        std::env::var_os("NODECRAFT_FFPROBE").as_deref(),
        std::env::var_os("PATH").as_deref(),
        cfg!(target_os = "macos"),
        executable,
    )
}

fn resolve(
    settings: &MediaSettings,
    ffmpeg: Option<&OsStr>,
    ffprobe: Option<&OsStr>,
    path: Option<&OsStr>,
    macos: bool,
    mut executable: impl FnMut(&std::path::Path) -> bool,
) -> Result<MediaSettings, &'static str> {
    let mut choose = |name: &str, override_path: Option<&OsStr>, configured: Option<&std::path::Path>| {
        // An invalid explicit choice must not silently run a different executable from PATH.
        if let Some(selected) = override_path.map(std::path::Path::new).or(configured) {
            if !selected.is_absolute() || selected.as_os_str().as_encoded_bytes().len() > MAX_PATH_BYTES {
                return Err("FFmpeg overrides and [media] settings must name absolute paths; fix NODECRAFT_FFMPEG / NODECRAFT_FFPROBE or the settings");
            }
            return Ok(Some(selected.to_path_buf()));
        }
        let filename = if cfg!(windows) { format!("{name}.exe") } else { name.to_owned() };
        if let Some(path) = path {
            if path.as_encoded_bytes().len() > MAX_PATH_BYTES {
                return Err("PATH is too long to search for FFmpeg; set absolute [media] paths or NODECRAFT_FFMPEG and NODECRAFT_FFPROBE");
            }
            for (index, directory) in std::env::split_paths(path).enumerate() {
                if index >= MAX_DIRECTORIES {
                    return Err("PATH has too many entries to search for FFmpeg; set absolute [media] paths or NODECRAFT_FFMPEG and NODECRAFT_FFPROBE");
                }
                // Relative and empty PATH entries could resolve inside an untrusted project folder.
                if !directory.is_absolute() {
                    continue;
                }
                let candidate = directory.join(&filename);
                if executable(&candidate) {
                    return Ok(Some(candidate));
                }
            }
        }
        // Finder/Dock launches usually omit Homebrew from launchd's PATH. These fixed absolute
        // locations are searched only after PATH, never after an explicit tool choice.
        if macos {
            for directory in ["/opt/homebrew/bin", "/usr/local/bin"] {
                let candidate = std::path::Path::new(directory).join(&filename);
                if executable(&candidate) {
                    return Ok(Some(candidate));
                }
            }
        }
        Ok(None)
    };
    Ok(MediaSettings { ffmpeg: choose("ffmpeg", ffmpeg, settings.ffmpeg.as_deref())?, ffprobe: choose("ffprobe", ffprobe, settings.ffprobe.as_deref())? })
}

fn executable(path: &std::path::Path) -> bool {
    let Ok(metadata) = path.metadata() else { return false };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    true
}

#[cfg(test)]
mod tests;
