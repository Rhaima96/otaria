# Installation and updates

Download the file for your system from the [releases page](https://github.com/Rhaima96/otaria-releases/releases/latest).

| System | File | Notes |
|---|---|---|
| macOS (Apple silicon and Intel) | `Otaria_<version>_universal.dmg` | Signed and notarized. Open the `.dmg` and drag Otaria to Applications. |
| Windows 10/11 | `Otaria_<version>_x64-setup.exe` or `.msi` | Not signed yet: Windows SmartScreen may warn. Click **More info**, then **Run anyway**. |
| Debian, Ubuntu | `Otaria_<version>_amd64.deb` | `sudo apt install ./Otaria_<version>_amd64.deb` |
| Fedora, RHEL, openSUSE | `Otaria-<version>-1.x86_64.rpm` | `sudo dnf install ./Otaria-<version>-1.x86_64.rpm` |
| Other Linux | `Otaria_<version>_amd64.AppImage` | `chmod +x` the file, then run it. |

Otaria works offline: it loads nothing from the web, except the update check.

## Updates

Otaria checks for a new version at startup. You can turn this off, or check by hand, from the **Updates** button. **Install and restart** downloads the signed update and restarts the app: open connections are closed and unsaved queries are lost.

## Optional: `mariadb-binlog`

The **Undo selection** feature of the [Binlog screen](administration.md#binlog) runs the `mariadb-binlog` client, which is not bundled. Install the MariaDB client tools (for example `brew install mariadb`, `apt install mariadb-client`, or the MariaDB Windows installer). Otaria looks for it in `PATH` and in the usual install folders.

## Where Otaria keeps its data

- Connection profiles: `connections.json` in the app's configuration folder (`~/Library/Application Support/dev.otaria.app` on macOS, `%APPDATA%\dev.otaria.app` on Windows, `~/.config/dev.otaria.app` on Linux).
- Passwords and passphrases: the system keychain only (macOS Keychain, Windows Credential Manager, Secret Service on Linux). They are never written to `connections.json`.
