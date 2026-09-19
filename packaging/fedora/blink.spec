%define _debugsource_template %{nil}
%define debug_package %{nil}

%global app_id io.github.sachesi.blink

Name:           blink
Version:        0.7.0
Release:        1%{?dist}
Summary:        Markdown editor with a live preview, for GNOME

# The KaTeX fonts math is typeset with are built into the binary.
License:        GPL-3.0-or-later AND OFL-1.1
URL:            https://github.com/sachesi/blink
Source0:        %{url}/archive/refs/tags/v%{version}.tar.gz#/%{name}-%{version}.tar.gz

BuildRequires:  cargo
BuildRequires:  rust >= 1.95
BuildRequires:  gcc
BuildRequires:  blueprint-compiler
BuildRequires:  desktop-file-utils
BuildRequires:  gettext
BuildRequires:  appstream
BuildRequires:  pkgconfig(gtk4) >= 4.20
BuildRequires:  pkgconfig(libadwaita-1) >= 1.8
BuildRequires:  pkgconfig(gtksourceview-5)
BuildRequires:  pkgconfig(glib-2.0)
BuildRequires:  pkgconfig(xkbcommon)

Requires:       gtk4%{?_isa} >= 4.20
Requires:       libadwaita%{?_isa} >= 1.8
Requires:       gtksourceview5%{?_isa}
Requires:       hicolor-icon-theme

%description
Blink is a Markdown editor for GNOME, built with GTK 4 and libadwaita. It shows
a document as source, as rendered text, or both side by side, in tabs, with find
and replace, a focus mode, and exports to PDF and standalone HTML. The preview
renders GitHub's Markdown, from task lists, alerts and footnotes to typeset math.
Unsaved work is saved to the file as you type, backed up for recovery after a
crash, and never written over a change another program made to the file.

%prep
%autosetup -n %{name}-%{version}

%build
export CARGO_HOME="$PWD/.cargo-home"
export RUSTFLAGS="%{?build_rustflags}"
export BLINK_LOCALEDIR="%{_datadir}/locale"
%if 0%{?_cargo_target_dir:1}
export CARGO_TARGET_DIR="%{_cargo_target_dir}"
%endif
cargo build --release --locked

%install
%if 0%{?_cargo_target_dir:1}
target="%{_cargo_target_dir}/release"
%else
target="target/release"
%endif
install -Dpm 0755 "$target/blink" %{buildroot}%{_bindir}/blink

install -d %{buildroot}%{_datadir}/applications %{buildroot}%{_metainfodir}
msgfmt --desktop --template=data/%{app_id}.desktop -d po \
  -o %{buildroot}%{_datadir}/applications/%{app_id}.desktop
msgfmt --xml --template=data/%{app_id}.metainfo.xml -d po \
  -o %{buildroot}%{_metainfodir}/%{app_id}.metainfo.xml
install -Dpm 0644 data/%{app_id}.gschema.xml %{buildroot}%{_datadir}/glib-2.0/schemas/%{app_id}.gschema.xml
install -Dpm 0644 data/icons/hicolor/scalable/apps/%{app_id}.svg \
  %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/%{app_id}.svg
install -Dpm 0644 data/icons/hicolor/symbolic/apps/%{app_id}-symbolic.svg \
  %{buildroot}%{_datadir}/icons/hicolor/symbolic/apps/%{app_id}-symbolic.svg

for lang in $(cat po/LINGUAS); do
  install -d %{buildroot}%{_datadir}/locale/$lang/LC_MESSAGES
  msgfmt -o %{buildroot}%{_datadir}/locale/$lang/LC_MESSAGES/%{name}.mo po/$lang.po
done
%find_lang %{name}

%check
desktop-file-validate %{buildroot}%{_datadir}/applications/%{app_id}.desktop
appstreamcli validate --no-net %{buildroot}%{_metainfodir}/%{app_id}.metainfo.xml
test -x %{buildroot}%{_bindir}/blink

%files -f %{name}.lang
%license LICENSE
%doc README.md docs
%{_bindir}/blink
%{_datadir}/applications/%{app_id}.desktop
%{_metainfodir}/%{app_id}.metainfo.xml
%{_datadir}/glib-2.0/schemas/%{app_id}.gschema.xml
%{_datadir}/icons/hicolor/scalable/apps/%{app_id}.svg
%{_datadir}/icons/hicolor/symbolic/apps/%{app_id}-symbolic.svg

%changelog
