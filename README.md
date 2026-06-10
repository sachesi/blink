# Blink

Blink is a fast, beautiful, and minimal Markdown editor built with Rust, GTK4, and libadwaita. It is designed to provide a distraction-free writing experience while offering powerful native features.

## ✨ Features

- **Live Split-View:** Instantly preview your Markdown as you type with a beautifully rendered 50-50 split screen.
- **Native Image Rendering:** Images are loaded directly from disk and embedded natively within the GTK preview window.
- **Focus Mode:** Press `F11` or use the menu to hide all toolbars and enter a fullscreen distraction-free zen mode.
- **Robust Auto-Save:** Automatically saves opened files in the background every 10 seconds to prevent data loss.
- **Rich Formatting:** Seamless parsing of tables, blockquotes, lists, and inline styles.
- **Export Capabilities:** Export your documents to **PDF** or **HTML** with a single click.
- **Modern UI:** Built on `libadwaita` for perfect integration with modern Linux desktops, including dark mode support.
- **Localization:** Fully translated into Ukrainian (`uk_UA`).

## 🛠️ Building from Source

To compile Blink from source, ensure you have the Rust toolchain installed, as well as the GTK4 and libadwaita development libraries for your distribution.

### Prerequisites (Ubuntu/Debian)
```bash
sudo apt install build-essential rustc cargo libgtk-4-dev libadwaita-1-dev libgtksourceview-5-dev gettext
```

### Build Instructions
Clone the repository and build the project using Cargo:
```bash
git clone https://github.com/sachesi/blink.git
cd blink
cargo build --release
```

The compiled binary will be located at `target/release/blink`.

## 📦 Installation (Linux Desktop)

To integrate Blink into your desktop environment (menus, icons, and translations):

1. Copy the compiled binary to your bin directory:
   ```bash
   sudo cp target/release/blink /usr/local/bin/
   ```

2. Install the desktop file and icons provided in the `packaging/` directory:
   ```bash
   sudo cp -r packaging/usr/share/applications/com.github.sachesi.blink.desktop /usr/share/applications/
   sudo cp packaging/usr/share/icons/hicolor/scalable/apps/com.github.sachesi.blink.svg /usr/share/icons/hicolor/scalable/apps/
   ```

3. Install compiled locales:
   ```bash
   sudo mkdir -p /usr/share/locale/uk/LC_MESSAGES
   msgfmt po/uk_UA.po -o /usr/share/locale/uk/LC_MESSAGES/blink.mo
   ```

## 📜 License

This project is open-source and available under the MIT License.
