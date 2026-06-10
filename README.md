# Blink

A GTK4/libadwaita Markdown editor written in Rust.

## Features

- Side-by-side Markdown preview
- Image rendering from local disk
- Focus mode (F11)
- Background auto-save (every 10s)
- PDF and HTML export
- Ukrainian localization (`uk_UA`)

## Build dependencies

- Rust toolchain
- `libgtk-4-dev`
- `libadwaita-1-dev`
- `libgtksourceview-5-dev`
- `gettext`

On Ubuntu/Debian:
```bash
sudo apt install build-essential rustc cargo libgtk-4-dev libadwaita-1-dev libgtksourceview-5-dev gettext
```

## Build

```bash
git clone https://github.com/sachesi/blink.git
cd blink
cargo build --release
```

## Install

Copy the binary and desktop integration files:

```bash
sudo cp target/release/blink /usr/local/bin/
sudo cp packaging/usr/share/applications/com.github.sachesi.blink.desktop /usr/share/applications/
sudo cp packaging/usr/share/icons/hicolor/scalable/apps/com.github.sachesi.blink.svg /usr/share/icons/hicolor/scalable/apps/
```

Compile and install the translations:

```bash
sudo mkdir -p /usr/share/locale/uk/LC_MESSAGES
msgfmt po/uk_UA.po -o /usr/share/locale/uk/LC_MESSAGES/blink.mo
```

## Maintainer

sachesi <xsachesi@pm.me>

## License

MIT
