# Compatibility Matrix

Use this checklist for manual real-world validation of text injection behavior.

## Browsers

- [ ] Safari (native text fields)
- [ ] Chrome (forms and contenteditable)
- [ ] Arc (forms and contenteditable)

## Editors

- [ ] VS Code
- [ ] Cursor
- [ ] TextEdit
- [ ] Notes

## Chat and Collaboration

- [ ] Slack
- [ ] Discord

## Terminals

- [ ] Terminal.app
- [ ] iTerm2

## Knowledge and Notes

- [ ] Notion
- [ ] Obsidian

## Per-app checks

- [ ] Short sentence dictation
- [ ] Multi-line dictation
- [ ] Punctuation handling
- [ ] Non-ASCII characters
- [ ] Long dictation (>20 seconds)
- [ ] Recovery after permission revocation

## Fallback behavior

- [ ] On injection failure, transcript still appears in app history
- [ ] Clipboard contains last transcript after injection attempt
