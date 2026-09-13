# Security

Please report a vulnerability privately rather than in a public issue: through
[a private advisory](https://github.com/sachesi/blink/security/advisories/new) on GitHub,
or by mail to sachesi <xsachesi@pm.me>. Say what you found, how to reproduce it and which
version you ran; a fix is worked out with you before anything is published.

Only the latest release gets fixes.

## What counts

Blink opens Markdown files that anyone may have written, and renders them as soon as they
are opened. The parts where a mistake matters most:

- The preview, which must not show a file from outside the document's folder, fetch
  anything from the network, or hand an address other than `http:`, `https:` or `mailto:`
  to the system (see [docs/usage.md](docs/usage.md)).
- The HTML export, which must not carry raw HTML, script or a dangerous address from the
  document into the page it writes.
- Saving, autosave and backups, which must not write anywhere but the file the user chose
  and their own backup folder, lose a document, or let other users read unsaved text.

A document that renders wrongly is a bug; please file it as an ordinary issue.
