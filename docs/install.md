# Installing NodeCraft

Downloads are on [GitHub Releases](https://github.com/CypherNaught-0x/nodecraft/releases). Each
release lists a `SHA256SUMS.txt`; compare a download's checksum against it if you want to be sure
the file is the one our CI built (`shasum -a 256 <file>` on macOS, `Get-FileHash <file>` in
PowerShell, `sha256sum <file>` on Linux).

> [!IMPORTANT]
> Until NodeCraft has code-signing certificates ([#100](https://github.com/CypherNaught-0x/nodecraft/issues/100)), the **macOS and Windows builds are not
> signed by a verified developer**. Your system will warn you the first time you open them. The
> steps below get past that warning once; after that the app opens normally.

## macOS

1. Download `nodecraft-<version>-macos-universal.dmg`, open it, and drag **NodeCraft** onto
   **Applications**.
2. Open NodeCraft from Applications (or Launchpad). macOS shows *"NodeCraft" Not Opened: Apple
   could not verify "NodeCraft" is free of malware…*. Click **Done** (not *Move to Trash*).
3. Open **System Settings › Privacy & Security** and scroll down to **Security**. Next to
   *"NodeCraft" was blocked to protect your Mac*, click **Open Anyway**, and confirm with your
   password or Touch ID.
4. In the dialog that follows, click **Open Anyway** again. From now on NodeCraft opens like any
   other app.

The *Open Anyway* button appears for about an hour after step 2; if it's gone, repeat step 2.
(Right-click › Open no longer skips this check on current macOS.)

**Command-line tool** (`nodecraft-cli-<version>-macos-universal.zip`): unzip it and run
`./nodecraft-cli --version` in Terminal. macOS blocks it the first time; go to **System Settings ›
Privacy & Security**, click **Allow Anyway** next to *nodecraft-cli*, run the command again and
choose **Open**. Alternatively, download and unpack it from Terminal, which doesn't trigger the
check at all:

```sh
curl -fLO https://github.com/CypherNaught-0x/nodecraft/releases/download/v<version>/nodecraft-cli-<version>-macos-universal.zip
unzip nodecraft-cli-<version>-macos-universal.zip
./nodecraft-cli-<version>-macos-universal/nodecraft-cli --version
```

## Windows

Pick the installer (`nodecraft-<version>-windows-x64.msi`, or `-arm64` for ARM PCs) or the portable
zip (`…-portable.zip`, no installation: unzip and run `nodecraft.exe`).

1. **Browser warning.** Edge may say the file *isn't commonly downloaded*. Click **…** next to the
   download › **Keep** › **Show more** › **Keep anyway**. (Chrome and Firefox may ask similarly;
   choose to keep the file.)
2. **SmartScreen.** When you run the installer or `nodecraft.exe`, Windows may show *Windows
   protected your PC*. Click **More info**, check that the app is NodeCraft, then click **Run
   anyway**. You only see this once per file.
3. The installer adds NodeCraft to the Start menu; the portable zip runs from wherever you unpacked
   it. The portable zip includes a `portable.txt`: while it sits next to `nodecraft.exe`, settings
   are kept in a `config` folder beside it instead of `%APPDATA%\nodecraft`.

**Smart App Control** (Windows 11): if it is turned on, Windows blocks unsigned apps outright and
offers no *Run anyway*. Until signed builds exist, the only way around it is turning Smart App
Control off (Windows Security › App & browser control › Smart App Control settings), which
Windows doesn't let you turn back on without resetting the PC. If you'd rather not, wait for
signed releases ([#100](https://github.com/CypherNaught-0x/nodecraft/issues/100)).

## Linux

No signing step is needed.

- **AppImage:** `chmod +x nodecraft-<version>-linux-x86_64.AppImage` and run it. On first run it
  adds itself to your application menu (set `NODECRAFT_NO_DESKTOP_INTEGRATION=1` to skip that).
- **Debian/Ubuntu:** `sudo apt install ./nodecraft-<version>-linux-x86_64.deb`
- **Fedora/openSUSE:** `sudo dnf install ./nodecraft-<version>-linux-x86_64.rpm` (or `zypper install`)
- **Flatpak:** `flatpak install --user nodecraft-<version>-linux-x86_64.flatpak`
- **Tarball:** unpack anywhere and run `bin/nodecraft`.

Use the `aarch64` files on ARM machines.

## Web (preview)

`nodecraft-web-<version>.zip` is a static site. Unzip it and serve the folder over HTTP from any
host or path, for example `python3 -m http.server` inside it, then open the page in a browser with
WebGPU or WebGL 2. Opening `index.html` from disk does not work, because browsers refuse to load
WebAssembly from `file://`. The browser can open and save projects, recover unsaved work and
paste PNG images (#118, #717, #633). It stores keys and provider server addresses per site.
Supported cards run when the provider allows requests from the app's origin. The **Providers**
panel configures a ComfyUI server and fetches its option lists in the background (#721).
See [provider and browser limits](providers.md).

### Live sharing in the browser

Open **Share** to start a room or join one. The browser uses the same `nodecraft-cli relay`
as the desktop. For local development, a page served over HTTP can connect to a relay on plain
WebSocket (`ws://`). For a remote relay used from an HTTPS page, put a TLS proxy in front of
the relay and use its `wss://` address. The desktop client uses `ws://`, so desktop peers
need a plain WebSocket address for the same room.

A relay keeps its rooms only in memory unless you start it with a store folder:
`nodecraft-cli relay --store <folder>` keeps each room, its share links and its media there, so
a restart loses nothing and a share link works while its owner is away. A room nobody joined for
`--keep-days` (default 30) is forgotten, and past `--store-max-gib` (default 8) the rooms used
longest ago go first. Folders are named by a hash of the room id; the id is never written.

The browser has a single window, so joining replaces the project in the current tab. It is refused
while that tab has changes of its own; save them first, or join in a new tab.

## Building from source

See [`development.md`](development.md): `cargo run --release -p nodecraft`.

## Updates

NodeCraft checks GitHub for a newer release at startup (turn it off with **Don't check at
startup**, or `[updates] check = false`). When the macOS app in Applications, the AppImage or the
Windows MSI install finds one, **Install update** downloads it in the background and checks its
signature; **Restart to update** then replaces this copy and reopens your project (unsaved changes
come back through crash recovery). Restarting waits while a generation runs. Other installs (the
deb, rpm, Flatpak, the portable zip) say where to get the new version instead. On Windows the
installer asks for permission as it does when you first install.

## Settings

NodeCraft keeps its settings in `config.toml`, which you can edit by hand (comments and unknown
keys are kept):

| Platform | Folder |
|---|---|
| macOS | `~/Library/Application Support/nodecraft` |
| Windows | `%APPDATA%\nodecraft` (portable zip: `config\` next to `nodecraft.exe`) |
| Linux | `$XDG_CONFIG_HOME/nodecraft` (usually `~/.config/nodecraft`); Flatpak: `~/.var/app/io.github.cyphernaught0x.nodecraft/config/nodecraft` |

Set `NODECRAFT_CONFIG_DIR` to use another folder, or to an empty value to keep settings in memory
only. A `portable.txt` next to the executable (next to `NodeCraft.app` on macOS) turns on portable
mode on any platform. A damaged file is renamed aside and the defaults are used; an invalid value
resets its section to the defaults, and the next save writes them over it.

```toml
schema_version = 1    # written by NodeCraft; leave it as it is
[appearance]
theme = "dark"        # or "light"

[canvas]
scroll = "pan"        # "zoom": the mouse wheel zooms, horizontal scroll still pans
zoom_speed = 1.0      # 0.25 to 4
```


## Optional FFmpeg tools

The desktop app plays videos on cards with a user-installed FFmpeg and ffprobe; it does not
bundle or install them. At startup it resolves each tool in this order: an absolute
`NODECRAFT_FFMPEG` / `NODECRAFT_FFPROBE` environment override, an absolute path in `[media]`
settings, then the first executable `ffmpeg` / `ffprobe` on `PATH` (`.exe` on Windows).
On macOS it then checks `/opt/homebrew/bin` and `/usr/local/bin`, so Homebrew tools are found
when Finder or the Dock starts the app without the shell’s `PATH`.
For explicit settings:

```toml
[media]
ffmpeg = "/usr/bin/ffmpeg"
ffprobe = "/usr/bin/ffprobe"
```

An invalid explicit path does not fall back to another executable. Discovery skips empty and
relative `PATH` entries, is bounded, and is not saved into settings; restart after changing
tools or the environment. Without the tools a video card explains how to install or configure
them. The same pair serves sound playback, video actions and Timeline rendering.
See [media-video.md](media-video.md) for capabilities, Flatpak/browser limitations and resource
contracts. Installing NodeCraft alone does not install a video toolchain.
