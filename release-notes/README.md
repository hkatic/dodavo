# Release notes

One folder per version, one plain-text file per language: `en.txt` (required), `hr.txt`, `sr-Latn.txt` and `sl.txt`. `installer\publish.ps1` puts them into the update manifest (`update.json`); the program shows the file of its language in the Update Available window and falls back to English. Keep them short and in plain text (no Markdown, the window shows them as they are), and start with what matters to the user.
