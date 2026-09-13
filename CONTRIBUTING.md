# Contributing

Bugs and ideas go to the [issue tracker](https://github.com/sachesi/blink/issues); security
problems do not, see [SECURITY.md](SECURITY.md).

Before a change goes in:

- `just check` and `just test` pass. CI runs both on Fedora 44, with `cargo deny check`,
  for every push and pull request.
- Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/):
  `fix:`, `feat:`, `perf:`, `docs:` and so on, with a subject that says what changed for
  someone using Blink.
- Every string the user sees goes through `gettext`. `just po` updates the catalogues in
  `po/`, and a change that adds strings brings their translations along where it can.
- Behaviour described in `docs/` changes with the code that implements it.

## Where things are

    build.rs               runs blueprint-compiler, bundles the GResource, compiles the
                           settings schema for the tests
    data/ui/*.blp          the window, the preferences and the shortcuts dialog
    data/style.css         structural CSS, colours come from libadwaita
    src/main.rs            locale and resources, then the application
    src/application.rs     AdwApplication subclass: app.* actions, accelerators, style
    src/window.rs          the window: template, win.* actions, view modes, settings; in
                           window/ the document's file and its command queue, crash
                           recovery, the preview, and find and replace
    src/preferences.rs     the preferences dialog, bound to GSettings
    src/markdown.rs        renders Markdown into the preview's text buffer
    src/export.rs          renders Markdown to a standalone HTML file
    src/conflict.rs        atomic writes and telling other programs' changes apart
    src/backup.rs          backup files, locks, and which backups are orphaned

Widgets are GObject subclasses with composite templates from the Blueprint files.
User actions are `GAction`s (`app.`, `win.`), so the menus, the accelerators and the
buttons reach the same code.

Anything that can wait on a dialog or the disk (opening, saving, autosave, backups, the
questions about other programs' changes and about closing) is a `Command` sent through one
queue in `window/document.rs`, and runs to its end before the next one starts. A timer or
a file monitor event therefore never lands in the middle of a save or of a question to the
user. File IO in those commands goes to a worker through `gio::spawn_blocking`.

`conflict.rs`, `backup.rs`, `export.rs` and the parsing half of `markdown.rs` have no GTK
state, and carry most of the unit tests.

## Running

    just run [FILE]      # debug build with the schema compiled into target/schemas
    just check           # fmt, clippy -D warnings, blueprint, validators, catalogues
    just test            # the unit tests
    cargo deny check     # advisories, licences and sources of the dependencies

A test compares the accelerators the application sets with the ones the shortcuts dialog
lists, so a new shortcut goes into both `ACCELS` in `src/application.rs` and
`data/ui/shortcuts_dialog.blp`.

## Translations

User-visible strings go through `gettext` with `{}` placeholders filled in by
`str::replacen`; counts use `ngettext` even where English would not need it, because the
plural rules of other languages do. `just pot` regenerates `po/blink.pot` from the Rust
sources, the Blueprint files, the desktop entry, the metainfo and the schema, and `just po`
merges it into every `po/<lang>.po`. A new language is a new line in `po/LINGUAS` plus
its `.po` file. `install` compiles the catalogues and merges the desktop and metainfo
translations with `msgfmt`.
