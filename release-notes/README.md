# Release notes

One folder per version, one plain-text file per language: `en.txt` (required), `hr.txt`, `sr-Latn.txt` and `sl.txt`. `installer\publish.ps1` and `macos/publish.sh` put them into the update manifests (`update.json`, `update-macos.json`); the program shows the file of its language in the Update Available window and falls back to English. Keep them in plain text (no Markdown, the window shows them as they are).

## Format

They are release notes, not an advertisement: a user who gets an update should see at a glance what is new in that version.

- The first line is the heading, then an empty line, then one list:

  | Language | Heading |
  | --- | --- |
  | `en` | Changes in 1.1.0 |
  | `hr` | Promjene u inačici 1.1.0 |
  | `sr-Latn` | Promene u verziji 1.1.0 |
  | `sl` | Spremembe v različici 1.1.0 |

- Each line of the list starts with its kind:

  | Language | New feature | Change | Fixed bug |
  | --- | --- | --- | --- |
  | `en` | `- New:` | `- Changed:` | `- Bugfix:` |
  | `hr` | `- Novo:` | `- Promjena:` | `- Ispravak:` |
  | `sr-Latn` | `- Novo:` | `- Promena:` | `- Ispravka:` |
  | `sl` | `- Novo:` | `- Sprememba:` | `- Popravek:` |

- New features come first, then changes, then fixed bugs. Within the new features, what users reported or asked for comes first.
- Name each fixed bug by itself, as the user saw it. Leave out fixes to what is new in the same version.
- Write impersonally: no first person, no greeting, no sentence about the release before the list and no closing line.
- The same files are shown on Windows and on a Mac, so say which system a line is about when it is not about both ("Windows: …", "Mac: …").
- While a release is in the making, do not say that a feature is missing on a system: it may still get it. Settle that in the final notes, before the release is tagged.
- Name controls and settings as the program does in that language (`src/i18n/`).
