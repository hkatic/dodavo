# VoiceOver checklist

To go through with VoiceOver on, on the current macOS and the oldest one at hand,
before a release. Automated checks (`ax_audit.swift`) only see that controls have
names, not what is actually spoken. Write what fails into `ISSUES.md`.

## Main window

- [ ] Opening Dodavo puts the focus on the list, which is announced as Recipients.
- [ ] Interacting with the list reads a recipient's name, Contact ID and transfer, and the arrow keys move between recipients.
- [ ] `Return` opens the Send panel for the selected recipient.
- [ ] `VO-Shift-M` on a recipient opens its menu, with Send…, Edit Recipient… and Remove Recipient.
- [ ] The toolbar can be reached, and each item says what it does. Send is reported as unavailable with no recipient selected.
- [ ] The status line can be read, and a change between online and offline is announced.
- [ ] ⌘N, ⌘I, ⌘⌫, ⇧⌘C, ⌘T, ⌘, and ⌘? work, and ⌘⌫ asks first.

## Add and Edit Recipient

- [ ] The sheet is announced with its title, and the focus is on Name.
- [ ] Each field reads its label. `Tab` goes Name, Contact ID, Cancel, Add.
- [ ] `Return` adds, `Esc` cancels. A wrong Contact ID gives an alert that is read, and leaves the sheet open.

## Sending

- [ ] The Send panel reads its message with the recipient's name. Files and folders can both be chosen, also several.
- [ ] Finder: Services > Send with Dodavo appears for a selected file, and the recipient question is read.
- [ ] Copying a file onto the Dock icon is not possible with VoiceOver, so the service must be enough.

## Incoming Transfer

- [ ] From an unknown sender, the window does not take the focus from the app in use, and the notification is read.
- [ ] Clicking the notification (`VO-M-M`, then its actions) goes to the window, which reads who sends what and the free space.
- [ ] `Return` accepts, `Esc` declines. The check box to add the sender reads its name.
- [ ] Choose… changes the folder, and the new free space is announced.
- [ ] From a recipient, the transfer starts by itself, with a notification that is read.

## Transfers window

- [ ] Each row reads as one sentence: item, other person, status, amount, speed, time remaining.
- [ ] `VO-Command-Space` on a row lists Pause or Resume and Cancel Transfer; after the end Open, Show in Finder and Remove from List.
- [ ] Interacting with a row reaches its buttons.
- [ ] While Dodavo is in front, each change of phase is announced; pausing on the other side is announced too.
- [ ] Pause while the focus is on it stays on a usable button; Cancel Transfer asks first, and the focus does not get lost when the button goes away.
- [ ] A failed transfer says so in words, not only with a mark.
- [ ] Clear Finished removes the rows, and the focus stays in the window.

## In the background

- [ ] Closing the main window leaves Dodavo running; the Dock icon brings it back.
- [ ] `VO-M-M` reaches the menu bar extra, which reads Dodavo and what the transfers are doing. Its menu reads as it should.
- [ ] A notification comes for each finished, paused and interrupted transfer while Dodavo is in the background.
- [ ] Quitting with a running transfer asks, with Quit and Continue Later, Cancel Transfers and Quit, and Don't Quit.

## Settings

- [ ] The tabs General and Receiving can be reached and are announced.
- [ ] Each field and check box reads its label. Changing one takes effect at once.
- [ ] Your Contact ID can be read, and Copy says that it was copied.
- [ ] Language says that it applies the next time Dodavo opens.

## Other windows

- [ ] About reads the version, the notice and the website, which opens.
- [ ] License: the pop-up button reads its label, and the text can be read from the start.
- [ ] Update Available: the release notes can be read. Install Update reports its progress in words.
- [ ] Help opens in the browser, in the language of the program, with headings to go by.
