# Blink

Blink is a Markdown editor for GNOME, written in Rust with GTK 4 and libadwaita. It shows a
document as source, as rendered text, or both side by side, and it keeps unsaved work
safe: a file is saved as you type, unsaved changes are backed up for recovery after a
crash, and a change another program made to the file is never overwritten without asking.

The preview renders tables, task lists whose boxes can be ticked, footnotes, GitHub's
alerts, emoji, collapsible details, typeset math, syntax-coloured code blocks and images
from the document's own folder. Find works in the source and in the rendered text,
replace in the source. Documents open in tabs, which can be dragged out into windows of
their own, or each in a window of its own if you prefer. A folder opened in Blink lists
its Markdown files, and those of the folders below it, in a sidebar. There is a focus
mode, full screen with nothing but the text, and an export to a standalone HTML file or
to PDF.

## Packages

Fedora 44, 45 and Rawhide, from the Copr project
[sachesi/software](https://copr.fedorainfracloud.org/coprs/sachesi/software/):

    sudo dnf copr enable sachesi/software
    sudo dnf install blink

openSUSE Tumbleweed and Slowroll, from the OBS project
[home:sachesi:software](https://build.opensuse.org/project/show/home:sachesi:software); for
Slowroll the address has `openSUSE_Slowroll` in it, and on aarch64 `openSUSE_Factory_ARM`:

    sudo zypper addrepo https://download.opensuse.org/repositories/home:sachesi:software/openSUSE_Tumbleweed/home:sachesi:software.repo
    sudo zypper install blink

Debian testing, from the same OBS project; Ubuntu 26.04 has an older Rust than Blink
needs:

    sudo install -d /etc/apt/keyrings
    curl -fsSL https://download.opensuse.org/repositories/home:sachesi:software/Debian_Testing/Release.key | sudo gpg --dearmor -o /etc/apt/keyrings/sachesi-software.gpg
    echo 'deb [signed-by=/etc/apt/keyrings/sachesi-software.gpg] https://download.opensuse.org/repositories/home:sachesi:software/Debian_Testing/ /' | sudo tee /etc/apt/sources.list.d/sachesi-software.list
    sudo apt update
    sudo apt install blink

Arch Linux: the AUR package `blink-markdown`, built from
[packaging/aur/PKGBUILD](packaging/aur/PKGBUILD), which each release tag updates.

The same packages are attached to each [release](https://github.com/sachesi/blink/releases).

## Building and installing

    just build
    sudo just install        # or: just prefix=$HOME/.local build install

Build needs Rust 1.95, `blueprint-compiler`, `just`, `gettext` and the development
packages for GTK 4.20, libadwaita 1.8 and GtkSourceView 5, or newer. Details, other
prefixes and removal are in [docs/installing.md](docs/installing.md).

## Documentation

- [Installing](docs/installing.md)
- [Using Blink](docs/usage.md), with the keyboard shortcuts and the settings
- [Contributing](CONTRIBUTING.md), including where things are in the code, and
  [reporting a vulnerability](SECURITY.md)

The interface is available in English and Ukrainian.

GPL-3.0-or-later.
