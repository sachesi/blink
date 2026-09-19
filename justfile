# Blink build and install tasks.
#
# `build` needs cargo and blueprint-compiler; `install` only copies what is already in
# target/release, so the two can run on different machines sharing this directory.
#
#   just build
#   sudo just install              (prefix /usr/local)
#   just prefix=$HOME/.local build install

set shell := ["bash", "-euo", "pipefail", "-c"]

app_id := "io.github.sachesi.blink"
prefix := env("PREFIX", "/usr/local")
destdir := env("DESTDIR", "")
bindir := destdir + prefix + "/bin"
datadir := destdir + prefix + "/share"
release := "target/release"
schema_dir := "target/schemas"
pot_dir := "target/pot"
check_dir := "target/check"
version := `sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml`

default:
    @just --list

# Release build, looking for its translations under the install prefix.
build:
    BLINK_LOCALEDIR={{prefix}}/share/locale cargo build --release

# Debug build.
build-debug:
    cargo build

# Compile the GSettings schema into target/schemas for running uninstalled.
schemas:
    mkdir -p {{schema_dir}}
    cp data/{{app_id}}.gschema.xml {{schema_dir}}/
    glib-compile-schemas {{schema_dir}}

# Run the debug build uninstalled: just run [FILE]
run *args: build-debug schemas
    GSETTINGS_SCHEMA_DIR={{schema_dir}} target/debug/blink {{args}}

# Lints: rustfmt, clippy, blueprint, desktop file, metainfo and catalogue validation.
check:
    cargo fmt --check
    cargo clippy --all-targets -- -D warnings
    mkdir -p {{check_dir}}
    blueprint-compiler batch-compile {{check_dir}} data/ui data/ui/*.blp >/dev/null
    desktop-file-validate data/{{app_id}}.desktop
    appstreamcli validate --no-net data/{{app_id}}.metainfo.xml
    glib-compile-schemas --strict --dry-run data
    for lang in $(cat po/LINGUAS); do msgfmt -c -o /dev/null po/$lang.po; done

# Regenerate po/blink.pot from the Rust sources, the Blueprint files, the desktop entry,
# the metainfo and the schema.
pot:
    mkdir -p {{pot_dir}}/ui
    blueprint-compiler batch-compile {{pot_dir}}/ui data/ui data/ui/*.blp >/dev/null
    # xgettext has no Rust mode; the C lexer copes once lifetimes ('a, 'static) are stripped.
    rm -rf {{pot_dir}}/src && cp -r src {{pot_dir}}/src
    find {{pot_dir}}/src -name '*.rs' -exec sed -i -E "s/'([A-Za-z_][A-Za-z0-9_]*)([^'A-Za-z0-9_]|$)/\1\2/g" {} +
    xgettext --from-code=UTF-8 --package-name=blink --package-version={{version}} \
        --msgid-bugs-address=https://github.com/sachesi/blink/issues \
        --language=C --keyword= --keyword=gettext --keyword=ngettext:1,2 \
        --flag=gettext:1:no-c-format --flag=ngettext:1:no-c-format --flag=ngettext:2:no-c-format \
        --add-comments=Translators --sort-by-file --directory={{pot_dir}} -o po/blink.pot $(cd {{pot_dir}} && find src -name '*.rs' | sort)
    xgettext -j --from-code=UTF-8 --package-name=blink --package-version={{version}} --msgid-bugs-address=https://github.com/sachesi/blink/issues --add-comments=Translators --sort-by-file --directory={{pot_dir}} -o po/blink.pot $(cd {{pot_dir}} && ls ui/*.ui)
    xgettext -j --from-code=UTF-8 --package-name=blink --package-version={{version}} --msgid-bugs-address=https://github.com/sachesi/blink/issues --language=Desktop --sort-by-file -o po/blink.pot data/{{app_id}}.desktop
    xgettext -j --from-code=UTF-8 --package-name=blink --package-version={{version}} --msgid-bugs-address=https://github.com/sachesi/blink/issues --sort-by-file -o po/blink.pot data/{{app_id}}.metainfo.xml data/{{app_id}}.gschema.xml

# Merge the current template into every po/<lang>.po.
po: pot
    for lang in $(cat po/LINGUAS); do msgmerge --update --backup=none --quiet po/$lang.po po/blink.pot; done
    for lang in $(cat po/LINGUAS); do msgfmt --statistics -o /dev/null po/$lang.po; done

# Unit tests.
test:
    cargo test

# Install the release build. Does not build: run `just build` first.
install:
    @test -x {{release}}/blink || { echo "error: {{release}}/blink missing; run 'just build' first" >&2; exit 1; }
    install -Dm755 {{release}}/blink {{bindir}}/blink
    mkdir -p {{datadir}}/applications {{datadir}}/metainfo
    msgfmt --desktop --template=data/{{app_id}}.desktop -d po -o {{datadir}}/applications/{{app_id}}.desktop
    msgfmt --xml --template=data/{{app_id}}.metainfo.xml -d po -o {{datadir}}/metainfo/{{app_id}}.metainfo.xml
    install -Dm644 data/{{app_id}}.gschema.xml {{datadir}}/glib-2.0/schemas/{{app_id}}.gschema.xml
    install -Dm644 data/icons/hicolor/scalable/apps/{{app_id}}.svg {{datadir}}/icons/hicolor/scalable/apps/{{app_id}}.svg
    install -Dm644 data/icons/hicolor/symbolic/apps/{{app_id}}-symbolic.svg {{datadir}}/icons/hicolor/symbolic/apps/{{app_id}}-symbolic.svg
    for lang in $(cat po/LINGUAS); do install -d {{datadir}}/locale/$lang/LC_MESSAGES; msgfmt -o {{datadir}}/locale/$lang/LC_MESSAGES/blink.mo po/$lang.po; done
    glib-compile-schemas {{datadir}}/glib-2.0/schemas
    update-desktop-database -q {{datadir}}/applications || true
    gtk4-update-icon-cache -qtf {{datadir}}/icons/hicolor || gtk-update-icon-cache -qtf {{datadir}}/icons/hicolor || true
    @echo "installed to {{prefix}}"

uninstall:
    rm -f {{bindir}}/blink
    rm -f {{datadir}}/applications/{{app_id}}.desktop {{datadir}}/metainfo/{{app_id}}.metainfo.xml
    rm -f {{datadir}}/glib-2.0/schemas/{{app_id}}.gschema.xml
    rm -f {{datadir}}/icons/hicolor/scalable/apps/{{app_id}}.svg {{datadir}}/icons/hicolor/symbolic/apps/{{app_id}}-symbolic.svg
    for lang in $(cat po/LINGUAS); do rm -f {{datadir}}/locale/$lang/LC_MESSAGES/blink.mo; done
    glib-compile-schemas {{datadir}}/glib-2.0/schemas || true
    update-desktop-database -q {{datadir}}/applications || true
    # A cache that still lists the removed icons hides the same icons installed elsewhere.
    gtk4-update-icon-cache -qtf {{datadir}}/icons/hicolor || gtk-update-icon-cache -qtf {{datadir}}/icons/hicolor || true
