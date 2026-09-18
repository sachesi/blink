# Blink

Blink is a Markdown editor for GNOME, written in Rust with GTK 4 and libadwaita. It shows a
document as source, as rendered text, or both side by side, and it keeps unsaved work
safe: a file is saved as you type, unsaved changes are backed up for recovery after a
crash, and a change another program made to the file is never overwritten without asking.

The preview renders tables, task lists whose boxes can be ticked, footnotes, GitHub's
alerts, emoji, collapsible details, typeset math, syntax-coloured code blocks and images
from the document's own folder. Find works in the source and in the rendered text,
replace in the source. Documents open in tabs, which can be dragged out into windows of
their own, or each in a window of its own if you prefer. There is a focus mode, full
screen with nothing but the text, and an export to a standalone HTML file or to PDF.

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
