<div align="center">

# 🐳 DSH Desktop

**DeepSeek Harness (DSH) on your desktop — a cross-platform app. Download it, double-click it, start chatting.**

![version](https://img.shields.io/badge/version-0.1.3-2f6fed)
![platform](https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-9cf)
[![CI](https://github.com/xiaoge200/dsh-desktop/actions/workflows/ci.yml/badge.svg)](https://github.com/xiaoge200/dsh-desktop/actions/workflows/ci.yml)
![PRs Welcome](https://img.shields.io/badge/PRs-welcome-brightgreen)

**English | [中文](README.md)**

</div>

---

## 📖 Table of Contents

- [What is this?](#-what-is-this)
- [Why build it?](#-why-build-it)
- [Features](#-features)
- [Quick Start](#-quick-start-beginners-tutorial)
- [FAQ](#-faq)
- [Developer Guide (Build from Source)](#-developer-guide-build-from-source)
- [Project Structure](#-project-structure-simplified)
- [More Docs](#-more-docs)
- [Contributing](#-contributing)
- [License](#-license)

---

## What is this? 🤔

DSH (DeepSeek Harness) is an AI agent tool that runs **entirely on your local machine**. Until now, using it meant installing Node.js first and then typing a string of commands in a terminal — for anyone unfamiliar with programming, that "set up the environment" step alone is enough to scare them off.

**DSH Desktop** takes care of all of that:

- Ready to use right out of the box — **no Node.js, no command line required**
- One-time initialization on first launch, then it opens **in seconds — fully offline-capable**
- **Auto-updates** when a new version of DSH is released, and automatically rolls back if an update fails. You never have to think about it.

In short: a zero-friction, desktop version of DSH.

## Why build it? 💡

| What trips beginners up | How we solve it |
|---|---|
| Installing Node.js and configuring the environment puts newcomers off | The Node runtime is bundled with the installer — double-click and done |
| First-time dependency installation takes half an hour | Ships with the complete DSH dependency bundle — usable from the very first launch |
| Command-line operations are incomprehensible | A GUI with plain-language prompts; technical details are folded away automatically |
| Upgrades are painful and easy to break | Background auto-updates with automatic rollback on failure — no disruption at all |

## ✨ Features

- 🖥️ **Cross-platform**: works on Windows / macOS / Linux
- 📦 **No environment setup**: Node runtime bundled in; nothing to install from the system
- ⚡ **Instant startup**: ready to use right after the one-time initialization, fully offline
- 🔄 **Auto-update**: new DSH versions install automatically; switches mirrors automatically when the network is unstable
- 🪟 **Dedicated window**: the DSH interface is embedded right in the app — no system browser needed
- 🧩 **System tray**: closing the window tucks the app into the tray; bring it back with one click
- 🌐 **Bilingual UI**: the interface language follows your system language automatically
- 🛡️ **Private & local**: the service runs only on your own machine, never exposed to the network
- 💬 **Beginner-friendly**: every message speaks plain language, so when something goes wrong you'll know what to do

## 🚀 Quick Start (Beginners' Tutorial)

> Never touched DSH before? Just follow these 3 steps.

### Step 1: Download the installer

1. Open the **Releases** page of this repository: <https://github.com/xiaoge200/dsh-desktop/releases>
2. Pick the latest release and download the installer for your platform:
   - **Windows**: `.msi` / `.exe` (Windows 10 or later, no admin rights needed)
   - **macOS**: `.dmg` (macOS 11 or later)
   - **Linux**: `.deb` / `.rpm` / `.AppImage` (requires the webkit2gtk runtime)
3. Double-click the installer and click through the prompts to finish.

### Step 2: Launch it

After installation, a **DSH Desktop** icon appears on your desktop — double-click to open it.

- The first launch performs a one-time initialization that takes **about one or two minutes** (depending on your machine); the progress page shows its status in real time.
- After that, every launch is instant.

### Step 3: Start using it

Once the interface loads, you're in the DSH workspace — just use it like any chat app.

**Everyday tips:**

| What you want to do | How |
|---|---|
| Minimize to the system tray | Click the **Close** (✕) button on the window |
| Open it again | Click the 🐳 icon in the system tray → **Open** |
| Launch at startup | Tray menu → **Settings** → enable **Launch at startup** |
| Turn off auto-updates | Tray menu → **Settings** → disable **Auto-update** |
| Quit completely | Tray menu → **Quit** |

## ❓ FAQ

**Q: Do I need to install Node.js?**
No. The Node runtime is bundled in the installer — install and go.

**Q: Where is my data stored?**
In your user directory (on Windows: `%APPDATA%\com.dsh.desktop`). Data is kept by default when you uninstall, and survives reinstalls.

**Q: How do I uninstall it?**
On Windows, find DSH Desktop in **Settings → Apps** and uninstall — no admin rights needed. Uninstalling removes only the app itself; your data is kept.

**Q: What if the port is already in use?**
Nothing to do. The app automatically switches to a free port — you'll never notice.

**Q: What happens if a DSH update fails?**
Nothing breaks. The old version is backed up before each update; if the new one can't install, the app **rolls back automatically** and retries on the next launch — without bothering you.

**Q: What if my antivirus blocks it or reports a false positive?**
If your antivirus blocks the app, add the installer or the installation directory to its allowlist and reinstall. If it still doesn't work, please report it in the Issues.

**Q: The UI shows in English/Chinese — can I switch it?**
The interface language follows your system language automatically (Chinese/English); no manual setting is needed.

## 🛠️ Developer Guide (Build from Source)

> Want to build it yourself, do secondary development, or contribute code? Read on. Regular users can skip this section.

**Prerequisites**: Rust 1.77+, Node 18+, plus the per-platform [Tauri system dependencies](https://tauri.app/start/prerequisites/).

```bash
# 1. Install dependencies
npm install

# 2. Run in development mode (with hot reload)
npm run tauri dev

# 3. Build the installers
npm run tauri build
```

> Note: `npm install` relies on npm scripts (`@tauri-apps/cli` and `esbuild` need their postinstall steps). If your environment enables npm's `allow-scripts` policy, you need to allow them in the `allow-scripts` list of your `.npmrc`.

## 🗂️ Project Structure (Simplified)

```
dsh-desktop/
├─ src/              # Frontend UI (startup progress page, settings page)
├─ src-tauri/        # Desktop shell (Rust / Tauri)
├─ resources/        # Bundled runtime resources (injected at build time)
├─ scripts/          # Build scripts
├─ docs/             # Release guide & implementation notes
└─ package.json
```

## 📚 More Docs

- [Release guide `docs/RELEASE.md`](docs/RELEASE.md) — how to publish a new release
- [Implementation notes `docs/IMPLEMENTATION.md`](docs/IMPLEMENTATION.md) — pitfalls and lessons from development
- [Changelog `CHANGELOG.md`](CHANGELOG.md)

## 🤝 Contributing

Any kind of contribution is welcome! You can:

- 🐛 Open an **Issue**: report a bug or suggest a new feature
- ✨ Submit a **PR**: fix a bug, add a feature, or improve the docs
- 💬 Share your experience in the Discussions

And if you like this project, starring it ⭐ is the biggest encouragement for us!

## 📄 License

This project is open-sourced under the **Apache License 2.0**. See [LICENSE](LICENSE).

---

*DSH Desktop is a desktop wrapper around DSH (DeepSeek Harness) and is not affiliated with DeepSeek.*
