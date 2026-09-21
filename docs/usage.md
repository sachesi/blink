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
the window open with the documents not yet closed. Closing the last tab closes the window,
unless the window shows a folder.

A window narrower than 700 pixels at the normal text size has no room for the tab bar. The
button with the number of tabs in the header bar shows all of them instead, to pick, close
or add one.

With "Open Documents in Tabs" turned off in Preferences, every new or opened document gets
a window of its own instead. Tabs can still be dragged between windows. Files picked in a
folder's sidebar are the exception: they always open as tabs of the folder's window.

## Folders

"Open Folder" in the main menu (Shift+Ctrl+O), a folder dropped on the window or a folder
passed to `blink` lists the Markdown files in the folder and in the folders below it in a
sidebar, so a repository's README and its `docs` are a click away. Opening a file on its own shows no sidebar and
looks at no other file. "Open Recent" lists the last ten folders above the last ten files.

Clicking a file in the sidebar opens it as a tab, or brings it to the front if it is open
already, and the file of the selected tab is highlighted. A name too long for the
sidebar shows in full when the pointer rests on it. The right-click menu of a row, also on the Menu key
or Shift+F10, opens a file in a window of its own, copies the path of a file or folder,
or shows it in the file manager. Hidden files and folders, such as
`.git` and `.github`, are left out, and so are `node_modules` and `target`. So are
folders reached through a symbolic link, which could lead back to where they started.
Folders with no Markdown in them anywhere are left out. The list stops at 5000 files, and
says so. It is read again whenever the window comes back to the front, and when a
document is saved under a new name.

F9, or the button at the left of the header bar, shows and hides the sidebar. Focus mode
hides it until you leave. In a window narrower than 700 pixels it covers the document,
and goes away once a file is picked.

A folder belongs to its window. A folder opened from a window that shows no folder yet
goes into that window, and an untitled document there that was never typed in gives way
to it; otherwise the folder gets a window of its own, and a folder that is shown already
is brought to the front. Closing every tab of the window leaves it open, on a page that
asks for a file from the sidebar. "Save As" of an untitled document starts in the folder.

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
  `www.example.com`, are links, and emoji shortcodes such as `:tada:` are emoji when a
  font has them, here and in the PDF, where an emoji written out that no font has shows as
  its shortcode instead; the HTML export always has the emoji.
- Alerts (`> [!NOTE]`, `[!TIP]`, `[!IMPORTANT]`, `[!WARNING]` and `[!CAUTION]`) start with
  their title in its colour.
- Front matter, the YAML between `---` lines or the TOML between `+++` lines at the top of a
  file, shows as a code block.
- Math between `$` signs is typeset in the line, and between `$$` signs on a centred line of
  its own, with the commands KaTeX knows, in the preview and in both exports. Letters
  KaTeX's fonts lack, such as Cyrillic in `\text{}`, are drawn in a font of the system. Math
  that does not parse, or has a character no font has, shows as its source. In a table cell
  a formula is set in the line, displayed or not.
- Definition lists, a term with `: its definition` on the next line, and wiki links,
  `[[Page]]` for `Page.md`.
- A footnote reference links to its note, and the note ends with an arrow that links back
  to the first reference.

Code blocks are coloured by GtkSourceView when their fence names a language it knows, or a
common alias of one (`js`, `py`, `sh`, `rs` and so on). Raw HTML is not laid out: its tags
are dropped and the text inside them is kept, without scripts and style sheets. The tags of
text styles still style it: `<b>`, `<strong>`, `<i>`, `<em>`, `<s>`, `<del>`, `<u>`, `<ins>`,
`<code>`, `<kbd>`, `<sup>`, `<sub>` and `<mark>`. A `<p>`, `<div>` or `<center>` starts a
line of its own, aligned as its `align` attribute says, centre or right. A `<div>` or
`<center>` aligns the Markdown after it up to its end tag, as a centred header written as
`<div align="center">`, a blank line, Markdown and `</div>`. An `<img>` shows
like a Markdown image, at the pixel width it gives, and a `<details>` element's `<summary>`
shows and hides the rest of it when clicked. It starts closed unless it has the `open`
attribute, and stays as it was left while the document is edited.

The box of a task list item can be ticked in the preview. It changes the `[ ]` or `[x]` in
the source, as typing it would. Ctrl+Z takes it back and Shift+Ctrl+Z does it again, in the
preview as in the source; in the preview they undo and redo any change to the source.

Text in code blocks and table cells is selected and copied like the rest of the preview, and
their right-click menu copies the selection or selects all of the block or cell. A
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
alternative text. Of raw HTML in the document only the text is kept, as in the preview, with
`<img>` taken as a Markdown image, and `<details>`, `<summary>`, the tags of text styles and
blocks written again without their attributes, but for `open`, an image's width and a
block's alignment. Link addresses other than web and mail addresses and relative paths are
left out too, so the page runs no script when a browser opens it. Headings carry the names that
links to `#a-heading` point at.

"Export as PDF…" sets the document on A4 pages in the same fonts, with page numbers and code
coloured as in the light style. The text can be selected and searched, web and mail links,
links to the document's headings and footnote references and their arrows back can be
followed, and the headings make the outline.
The content of `<details>` elements is set open. Code lines too long for the page wrap, and
a table too wide for its words is set in smaller text, down to 8 points, before its words
break.
As in the preview, only images from the document's folder are shown.

## Keyboard shortcuts

Ctrl+? lists them all.

| Keys | |
|---|---|
| Ctrl+N | New document |
| Ctrl+O | Open |
| Shift+Ctrl+O | Open a folder |
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
| F9 | Show or hide the folder's sidebar |
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
