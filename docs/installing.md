# Installing

## What you need

To build: Rust 1.95 or newer, `blueprint-compiler`, `just`, `gettext`, and the development
packages of GTK 4.20, libadwaita 1.8 and GtkSourceView 5, or newer. On Fedora that is
`gtk4-devel libadwaita-devel gtksourceview5-devel blueprint-compiler just gettext`; on
Debian `libgtk-4-dev libadwaita-1-dev libgtksourceview-5-dev blueprint-compiler just
gettext`. `just check` also wants `desktop-file-validate` and `appstreamcli`.

To run: GTK 4.20, libadwaita 1.8 and GtkSourceView 5.

## Build

    just build          # release
    just build-debug
    just check          # rustfmt, clippy, blueprint, desktop file and metainfo validation
    just run notes.md   # debug build, uninstalled

The locale directory is compiled in. `just build` sets it to `share/locale` under the
prefix it will install to, so build and install with the same prefix. A plain
`cargo build` uses `/usr/local/share/locale`; set `BLINK_LOCALEDIR` in its environment for
another one.

Blink reads its settings from GSettings, which needs the schema installed. `just run`
compiles it into `target/schemas` and points the debug build at it; a binary started any
other way before `just install` stops with "Settings schema not installed".

## Install

    sudo just install
    just prefix=$HOME/.local build install
    DESTDIR=/tmp/stage just install

`install` copies what `just build` produced; it never builds. The default prefix is
`/usr/local`. It puts the binary in `bin`, and the desktop entry, metainfo, icons,
GSettings schema and translations under `share`, then compiles the schemas and refreshes
the desktop and icon caches. A prefix other than `/usr/local` and `/usr` needs its `share`
directory in `XDG_DATA_DIRS` for the desktop to find the entry; `~/.local/share` is on
most systems.

Packages for Fedora, openSUSE, Debian, Ubuntu and Arch Linux, and how to install them, are in
the [README](../README.md#packages).

## Removing

    sudo just uninstall

Your settings stay in dconf under `/io/github/sachesi/blink/`, and backups of unsaved work,
if any are left, in `~/.local/state/blink`.
