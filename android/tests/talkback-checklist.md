# TalkBack checklist

To go through with TalkBack on, on a phone with the current Android and on the
oldest Android at hand, before a release. The automated checks
(`ScreensAccessibilityTest`, Compose's accessibility checks) see names, touch
targets and contrast, not what is actually spoken. Write what fails into
`ISSUES.md`.

## The first start

- [ ] Android's questions (notifications, nearby devices, files on Android 8 to 10) are read, and the Recipients screen is read after them.
- [ ] The status line says "Connecting to the network…" from the start, and "Online – ready to send and receive files." is read once it says so, without the focus moving. Listen for whether "Connecting to the network…" is read as well when the screen opens.
- [ ] Without internet, the status line says "Connecting to the network…" for about 5 seconds, and then "Offline – check your internet connection. Transfers within your local network still work." is read.

## Recipients

- [ ] Swiping goes: status line, the text about having no recipients (or the recipients), Add recipient, the navigation bar.
- [ ] Each recipient is read as one item with its name and short Contact ID, or its transfer; double-tap says "Send files" and opens the file picker.
- [ ] The button after each recipient is read as "More options for" and the name; its menu reads Send files, Send a folder, Select, Edit, Copy Contact ID, Block and Remove.
- [ ] Select in a recipient's menu starts the selection: the top bar reads Clear selection, "1 selected" and Send, and each recipient is read as a check box, checked or not checked, which a double-tap changes. The number selected is read when it changes, without the focus moving.
- [ ] Outside the selection, a recipient offers "Select" for a double-tap and hold, and in TalkBack's actions menu.
- [ ] Send in the selection reads Send files and Send a folder; the chosen items appear on the Transfers screen once for each selected recipient. Clear selection and Back leave the selection, and Add recipient is back.
- [ ] Remove and Block ask first; the dialog is read with its title and text, and the recipient then leaves the list.
- [ ] More options at the top reads Share my Contact ID, Copy my Contact ID, Settings, Help and About Dodavo. Copying is confirmed (by Android on 13 and later).

## Add and Edit recipient

- [ ] The screen is read with its title; the fields read their labels and help, Paste puts in a copied ID.
- [ ] A wrong Contact ID is read as an error on the field, and the screen stays open.

## Sending

- [ ] The file picker opens, and a chosen file appears on the Transfers screen as a transfer of its own.
- [ ] Send a folder opens the folder picker; Android's question about access is read.
- [ ] Sharing from another app opens Send files with the recipients, and a double-tap sends.
- [ ] There, Select at the top turns the recipients into check boxes; Send is unavailable until one is checked, and then sends to all that are.

## Incoming transfer

- [ ] With Dodavo in front, the status line says so and a card appears, without the focus moving; Incoming transfer on the card opens the screen.
- [ ] With Dodavo in the background, the notification is read; Review opens the screen, Decline declines.
- [ ] The screen reads who sends what, the free space and Download/Dodavo; the check box to add an unknown sender reads its name.
- [ ] Accept and Decline work; More options has Block sender, which asks first.

## Transfers

- [ ] Each card is read as one item: item, person, status, amount, speed and time left, then its buttons.
- [ ] Pause and Resume, Cancel transfer (which asks first), Open, Share, Show folder and Remove from list are read and work.
- [ ] Changes in what a transfer is doing are read from the status line while Dodavo is in front, and come as notifications otherwise.

## Settings, Help and About

- [ ] Each setting reads its name, its state and its help; the switches and choices are toggled with a double-tap.
- [ ] Blocked senders lists them, each with Unblock (read with the name).
- [ ] Language opens Android's own choice on 13 and later, Dodavo's list before.
- [ ] Help reads as a web page: headings with the reading controls, links, and the list of contents.
- [ ] About reads the version, copyright and the notice; Licenses switches between Dodavo's and the components' licenses.

## Largest text and keyboard

- [ ] At the largest font size and display size, no text is cut and every screen scrolls.
- [ ] With a keyboard, Tab reaches every control in order, Enter activates, and Escape goes back.
