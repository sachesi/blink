# Using Blink

## Views

The three toggles in the header bar show the source, the rendered preview, or both side by
side. In the split view the two scroll together. A window narrower than 700 pixels at the
normal text size has no room for two panes: the split view gives way to the view it was
opened from, and comes back only once the window is wider and it is picked again.

A file opens in the preview, a new document in the source.

On a wide window the source and the preview stay in a column of 700, 800, 900 or 1000
pixels, 900 unless changed. The button beside the view toggles changes it for the window;
picked in Preferences, it applies to the window and to every window opened later.

Focus mode (F11, or the main menu) puts the window in full screen without the header bar
and the status bar; F11 or Escape leaves it.

## The preview

The preview renders CommonMark with tables, strikethrough, task lists and footnotes. Code
blocks are coloured by GtkSourceView when their fence names a language it knows, or a
common alias of one (`js`, `py`, `sh`, `rs` and so on). Raw HTML is not laid out: its tags
are dropped and the text inside them is kept.

Text in code blocks and table cells is selected and copied like the rest of the preview. A
code block also has a button that copies all of it, shown while the pointer is over the
block. A table wider than the preview scrolls sideways.

Documents can come from anyone, so the preview is careful with what they point at:

- Images are shown only when they are files inside the document's own folder or below
  it. Anything else, including web addresses, shows the image's alternative text. An
  untitled document shows no images.
- Links open in the default browser or mail program when they are `http:`, `https:` or
  `mailto:` addresses, and do nothing otherwise.

## Saving and your work

Changes to a document that has a file are written to it every ten seconds, and when you
save. A document without a file is only written when you save it.

A few seconds after you stop typing, and when the window loses the focus, unsaved changes
are copied to a backup in `~/.local/state/blink/backups`, readable only by you. When Blink
starts and finds a backup left by a session that ended without saving, it offers to
restore it. Discard deletes the backup; closing the question keeps it for the next start.

When another program changes the open file, Blink asks whether to reload it, overwrite it
with the text in the window, or save the text somewhere else, and does not save
automatically until you have chosen. If the file is deleted, the document stays open
without a file, and the next save asks where to.

Saving writes a new file and renames it over the old one, so a crash never leaves half a
document. A file reached through a symbolic link is written where the link points, and
the link stays.

## Export

"Export as HTML…" in the main menu writes a standalone page laid out like the preview: a
column as wide as the window's, the text and monospace fonts, and code coloured for the
light or the dark style, whichever the browser asks for. Images from the document's folder
are embedded in the page, web images are left as addresses, and any other image shows its
alternative text. Raw HTML in the document is left out, and so are link addresses other
than web and mail addresses and relative paths, so the page runs no script when a browser
opens it.

## Keyboard shortcuts

Ctrl+? lists them all.

| Keys | |
|---|---|
| Ctrl+N | New document |
| Ctrl+O | Open |
| Ctrl+S | Save |
| Shift+Ctrl+S | Save as |
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
fonts, word wrap, line numbers, the highlight of the line with the cursor, the shading of
every other line and the tab width. The text font is the one of the preview, the
monospace font the one of the editor and of code; either follows the system's document or
monospace font until another is picked, and the button beside it goes back to that.
Everything is stored in GSettings under `io.github.sachesi.blink`. The text size set with
Ctrl++ and Ctrl+- is `zoom`, in points above or below 11.
