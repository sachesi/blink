# Using Blink

## Tabs and windows

Documents open as tabs of the window they are opened from: a new document, a file from the
Open dialog or Open Recent, a file dropped on the window, and a file passed to `blink` while
it is running. The tab bar shows once a window holds more than one document. A file that is
already open, under its own name or through a symbolic link, is brought to the front
instead of being opened a second time, and a file opened while the window shows an untitled
document that was never typed in takes its place. For the same reason "Save As" refuses a
file that is open in another tab or window. A tab's tooltip shows the path of its file, and
while two tabs of a window hold files of the same name, each title also names its folder.

A tab dragged out of its window and dropped outside every window opens in a window of its
own, and dropped on another window's tab bar joins it. "Move to New Window" in a tab's
context menu does the same without dragging. Closing a window closes its documents one
after the other, each asking about unsaved changes first; cancelling that question leaves
the window open with the documents not yet closed. Closing the last tab closes the window.

A window narrower than 700 pixels at the normal text size has no room for the tab bar. The
button with the number of tabs in the header bar shows all of them instead, to pick, close
or add one.

With "Open Documents in Tabs" turned off in Preferences, every new or opened document gets
a window of its own instead. Tabs can still be dragged between windows.

## Views

The three toggles in the header bar show the source, the rendered preview, or both side by
side. The view is the window's: switching tabs keeps it. In the split view the two scroll
together. A window narrower than 700 pixels at the normal text size has no room for two
panes: the split view gives way to the view it was opened from, and comes back only once
the window is wider and it is picked again. The toggles move to the bottom of such a
window, to leave room for the title.

A file opens in the preview, a new document in the source.

On a wide window the source and the preview stay in a column of 700, 800, 900 or 1000
pixels, 900 unless changed. The button beside the view toggles changes it for the window
and its tabs; picked in Preferences, it applies to the window and to every window opened
later. A tab moved to another window takes that window's width.

Focus mode (F11, or the main menu) puts the window in full screen without the header bar
and the status bar; F11 or Escape leaves it.

## The preview

The preview renders CommonMark with tables, strikethrough, task lists and footnotes, and
more of what GitHub renders:

- Web addresses written out in the text, such as `https://example.com` or
  `www.example.com`, are links, and emoji shortcodes such as `:tada:` are emoji.
- Alerts (`> [!NOTE]`, `[!TIP]`, `[!IMPORTANT]`, `[!WARNING]` and `[!CAUTION]`) start with
  their title in its colour.
- Front matter, the YAML between `---` lines or the TOML between `+++` lines at the top of a
  file, shows as a code block.
- Math between `$` signs shows as inline code, and between `$$` lines as a LaTeX code
  block; it is not typeset.
- Definition lists, a term with `: its definition` on the next line, and wiki links,
  `[[Page]]` for `Page.md`.
- A footnote reference links to its note, and the note ends with an arrow that links back
  to the first reference.

Code blocks are coloured by GtkSourceView when their fence names a language it knows, or a
common alias of one (`js`, `py`, `sh`, `rs` and so on). Raw HTML is not laid out: its tags
are dropped and the text inside them is kept, except for `<img>`, which shows like a
Markdown image, and `<details>`, whose `<summary>` shows and hides the rest of it when
clicked. It starts closed unless it has the `open` attribute, and stays as it was left while
the document is edited.

The box of a task list item can be ticked in the preview. It changes the `[ ]` or `[x]` in
the source, as typing it would. Ctrl+Z takes it back and Shift+Ctrl+Z does it again, in the
preview as in the source; in the preview they undo and redo any change to the source.

Text in code blocks and table cells is selected and copied like the rest of the preview. A
code block also has a button that copies all of it, shown while the pointer is over the
block. A table wider than the preview scrolls sideways.

Documents can come from anyone, so the preview is careful with what they point at:

- Images are shown only when they are files inside the document's own folder or below
  it. Anything else, including web addresses, shows the image's alternative text. An
  untitled document shows no images.
- Links open in the default browser or mail program when they are `http:`, `https:` or
  `mailto:` addresses. A link to `#a-heading` scrolls to the heading, named as GitHub names
  them: in lower case, spaces made hyphens and punctuation left out. A link to a Markdown
  file by its path from the document's folder opens the file in Blink, without going to
  a heading of it. Other links do nothing.

## Saving and your work

Changes to a document that has a file are written to it every ten seconds, and when you
save. A document without a file is only written when you save it.

A few seconds after you stop typing, and when the window loses the focus, unsaved changes
are copied to a backup in `~/.local/state/blink/backups`, readable only by you. When Blink
starts and finds a backup left by a session that ended without saving, it offers to
restore it. Discard deletes the backup; closing the question keeps it for the next start.

When another program changes the open file, Blink asks whether to reload it, overwrite it
with the text in the window, or save the text somewhere else, and does not save
automatically until you have chosen. For a tab that is not selected, the question waits
until you select it, and the tab is marked meanwhile. If the file is deleted, the document
stays open without a file, and the next save asks where to.

Saving writes a new file and renames it over the old one, so a crash never leaves half a
document. A file reached through a symbolic link is written where the link points, and
the link stays.

## Export

"Export as HTML…" in the main menu writes a standalone page laid out like the preview: a
column as wide as the window's, the text and monospace fonts, and code coloured for the
light or the dark style, whichever the browser asks for. Images from the document's folder
are embedded in the page, web images are left as addresses, and any other image shows its
alternative text. Raw HTML in the document is left out, except for `<img>`, taken as a
Markdown image, and for `<details>` and `<summary>`, written without their attributes but
`open`. Link addresses other than web and mail addresses and relative paths are left out
too, so the page runs no script when a browser opens it. Headings carry the names that
links to `#a-heading` point at.

"Export as PDF…" sets the document on A4 pages in the same fonts, with page numbers and code
coloured as in the light style. The text can be selected and searched, web and mail links,
links to the document's headings and footnote references and their arrows back can be
followed, and the headings make the outline.
The content of `<details>` elements is set open. Code lines too long for the page wrap.
As in the preview, only images from the document's folder are shown.

## Keyboard shortcuts

Ctrl+? lists them all.

| Keys | |
|---|---|
| Ctrl+N | New document |
| Ctrl+O | Open |
| Ctrl+S | Save |
| Shift+Ctrl+S | Save as |
| Ctrl+W | Close the document |
| Ctrl+Page Down, Ctrl+Page Up | Next and previous tab; Ctrl+Tab and Shift+Ctrl+Tab too |
| Shift+Ctrl+Page Down, Shift+Ctrl+Page Up | Move the tab right or left |
| Alt+1 to Alt+9 | Go to a tab |
| Ctrl+Z, Shift+Ctrl+Z | Undo and redo, in the source and in the preview |
| Ctrl+B, Ctrl+I, Ctrl+K | Bold, italic, link around the selection, in the source |
| Ctrl+F | Find, or close the search |
| Ctrl+G, Shift+Ctrl+G | Next and previous match |
| Ctrl+H | Replace |
| Shift+Ctrl+H | Replace all |
| Ctrl++, Ctrl+-, Ctrl+0 | Larger text, smaller text, normal size |
| F11 | Focus mode |
| Ctrl+, | Preferences |
| Ctrl+Q | Quit |

## Settings

Preferences holds the style (follow the system, light or dark), the content width, the
fonts, whether documents open in tabs or in windows of their own, word wrap, line numbers, the highlight of the line with the cursor, the shading of
every other line and the tab width. The text font is the one of the preview, the
monospace font the one of the editor and of code; either follows the system's document or
monospace font until another is picked, and the button beside it goes back to that.
Everything is stored in GSettings under `io.github.sachesi.blink`. The text size set with
Ctrl++ and Ctrl+- is `zoom`, in points above or below 11.
