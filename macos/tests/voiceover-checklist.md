# VoiceOver checklist

To go through with VoiceOver on, on the current macOS and the oldest one at hand,
before a release. Automated checks (`ax_audit.swift`) only see that controls have
names, not what is actually spoken. Write what fails into `ISSUES.md`.

## The first start

- [ ] Opening a downloaded copy reads macOS's question, whether to open an app downloaded from the Internet.
- [ ] A moment after the window opens, the question about moving to the Applications folder is read, not only the button that has the focus. Moving it does not make macOS ask again.

## Main window

- [ ] Opening Dodavo puts the focus on the list, which is announced as Recipients. The status comes after that: "Online – ready to send and receive files.", and never "Offline" first while the connection is there.
- [ ] The same when the window opens later: with "Hide when opened at login" on, start Dodavo with `--minimized` and without internet, and open the main window about 4 seconds after that. The window is read first, and "Offline – check your internet connection. Transfers within your local network still work." after it.
- [ ] Interacting with the list reads a recipient's name, Contact ID and transfer, and the arrow keys move between recipients.
- [ ] `Return` opens the Send panel for the selected recipient.
- [ ] `VO-Command-Return` on a second recipient selects it as well, and VoiceOver says so; `Shift` with the arrow keys does the same without VoiceOver. `Return` then opens the Send panel, whose message reads the names of all selected recipients.
- [ ] With several recipients selected, Edit Recipient…, Remove Recipient and Block Recipient are reported as unavailable, and the menu of the selection has only Send… and Chat.
- [ ] `VO-Shift-M` on a recipient opens its menu, with Send…, Chat, Edit Recipient…, Remove Recipient and Block Recipient.
- [ ] Block Recipient asks first, with an alert that is read, and the recipient then leaves the list.
- [ ] Command-B does the same as Block Recipient, and does nothing without a selected recipient.
- [ ] The toolbar can be reached, and each item says what it does. Send and Chat are reported as unavailable with no recipient selected.
- [ ] The status line can be read, and a change between online and offline is announced.
- [ ] ⌘N, ⌘I, ⌘⌫, ⇧⌘C, ⇧⌘M, ⌘T, ⌘, and ⌘? work, and ⌘⌫ asks first.

## Add and Edit Recipient

- [ ] The sheet is announced with its title, and the focus is on Name.
- [ ] Each field reads its label. `Tab` goes Name, Contact ID, Largest item, Cancel, Add; with This size chosen, Largest size and Unit come after Largest item.
- [ ] Largest item reads as a pop-up button with its choice, and Largest size and Unit read their names. They are unavailable unless This size is chosen.
- [ ] `Return` adds, `Esc` cancels. A wrong Contact ID, or a size that is not a whole number from 1 on, gives an alert that is read, and leaves the sheet open.

## Sending

- [ ] The Send panel reads its message with the recipient's name. Files and folders can both be chosen, also several.
- [ ] Finder: Services > Send with Dodavo appears for a selected file, and the recipient question is read.
- [ ] The recipient question has a list that is read with its name, with the recipients selected in the main window selected in it; several can be selected with `VO-Command-Return`, and `Return` sends to all of them.
- [ ] The recipient question sends to the recipient it shows, also when the list changed while it was open. If that recipient was removed or blocked meanwhile, an alert that is read says so, and nothing is sent.
- [ ] Copying a file onto the Dock icon is not possible with VoiceOver, so the service must be enough.

## Incoming Transfer

- [ ] From an unknown sender, the window does not take the focus from the app in use, and the notification is read.
- [ ] Clicking the notification (`VO-M-M`, then its actions) goes to the window, which reads who sends what and the free space.
- [ ] `Return` accepts, `Esc` declines. The check box to add the sender reads its name.
- [ ] Choose… changes the folder, and the new free space is announced.
- [ ] From a recipient, the transfer starts by itself, with a notification that is read.
- [ ] Block Sender can be reached with `Tab` and reads its title. Its alert is read, Block closes the window, and the sender's other offers that were waiting close too.
- [ ] An offer from a blocked sender shows nothing at all, and the sender sees it declined.
- [ ] Command-B in the window of an offer does the same as Block Sender.
- [ ] Chat can be reached with `Tab`, and opens the chat with the sender; the offer stays as it is, and can be answered afterwards.
- [ ] An item over the sender's size limit waits for an answer, also from a recipient, and its window has a line that says so, read after the item's name and size.
- [ ] With "Decline it and tell me", an item over the limit shows no window, and a notification that is read says who offered what, also while Dodavo is in the background.
- [ ] Several items from one sender (select several files on the other side) share one window: it reads how many items there are and their total size, and the Items table reads each item with the arrow keys. `Return` accepts all of them, `Esc` declines all of them.
- [ ] An item that arrives while that window is the key window is announced once, with the new number of items; while another window or app is in front, a notification that is read tells of it, and not both.
- [ ] When the sender withdraws one of several items, the window stays with the others, and it closes with a notification when the last one is withdrawn.
- [ ] An offer that arrives while the window asks something (the Block Sender alert, or Choose…) opens a window of its own beside it, which can be answered by itself.

## Transfers window

- [ ] Each row reads as one sentence: item, other person, status, amount, speed, time remaining.
- [ ] `VO-Shift-M` on a row opens a menu with Pause or Resume, Cancel Transfer and Chat; after the end Open, Show in Finder and Remove from List.
- [ ] Shift-Command-M opens the chat with the other person of the selected row, as its Chat button does.
- [ ] Interacting with a row reaches its buttons, and `VO-Command-Space` there lists each of them once.
- [ ] Without VoiceOver: `Space` pauses or resumes the selected row, `Delete` cancels or removes it, `Return` opens what was saved.
- [ ] While Dodavo is in front, each change of phase is announced; pausing on the other side is announced too.
- [ ] Pause while the focus is on it stays on a usable button; Cancel Transfer asks first, and the focus does not get lost when the button goes away.
- [ ] A failed transfer says so in words, not only with a mark.
- [ ] Clear Finished removes the rows, and the focus stays in the window.

## Chat

- [ ] Shift-Command-M on a recipient opens Chat with their name, with the focus in Message. The window is read with its title; for someone who is no recipient, the line above the conversation says so.
- [ ] `Return` sends what Message holds and empties it, and the message is the last row of Conversation. With Message empty, `Return` sends nothing.
- [ ] Conversation reads as a table; interacting with it and the arrow keys read one message per row, as "name, time: text", "You, time: text" or "You, time (not sent): text", also a long one that wraps. Command-C copies the selected rows.
- [ ] A message that arrives while the chat is in front is announced once, as "name: text", and so are several that arrive together; no notification comes for it. Its sound plays.
- [ ] A message that arrives while another window or another app is in front opens the chat or leaves it where it is, without taking the focus, and a notification that is read tells of it, with nothing announced besides; the notification's own sound does not play on top of Dodavo's. Clicking it goes to the chat.
- [ ] "… is typing…" is announced once, with its sound, while the chat is in front, and the status line loses it when the other person stops; with "Announce when the other person is typing" off it only shows.
- [ ] A message to someone whose Dodavo is not running is marked as not sent after a moment, the status line says why and is announced, and the text is back in Message. With the window closed meanwhile, a notification says it, and the conversation shows it as not sent when opened again.
- [ ] Add to My Recipients… opens the sheet with name and Contact ID filled in; after Add, the title has the new name, and the line about a person who is no recipient and the button are gone, with the focus in Message.
- [ ] Block asks first, with an alert that is read, and the chat closes. Blocking the person elsewhere closes it too.
- [ ] Delete Conversation… asks first; afterwards the table is empty, the button is unavailable, the focus is in Message, and "The conversation was deleted." is announced.
- [ ] With several recipients selected, Shift-Command-M asks for one message: the sheet reads its title, the recipients and the note, the field reads as Message, and Send is unavailable while it is empty. Each of them gets the message, and none sees the others.

## In the background

- [ ] Closing the main window leaves Dodavo running; the Dock icon brings it back.
- [ ] `VO-M-M` reaches the menu bar extra, which reads Dodavo and what the transfers are doing. Its menu reads as it should.
- [ ] A notification comes for each finished, paused and interrupted transfer while Dodavo is in the background.
- [ ] Quitting with a running transfer asks, with Quit and Continue Later, Cancel Transfers and Quit, and Don't Quit.

## Settings

- [ ] The tabs General, Receiving, Chat, Sounds and Blocked can be reached and are announced.
- [ ] Chat: "Who can send me messages" reads as a pop-up button with its choice, and the two check boxes read their titles. Delete All Conversations… is unavailable while none is kept; it asks first, the focus then goes to the pop-up button, "All conversations were deleted." is announced, and the open chats are empty.
- [ ] Sounds: "Play sounds" reads as a check box. The table reads as Events, each row as the event and what it plays. Sound reads as a pop-up button with Default, None, a chosen file by its name, and Choose…, whose panel takes sound files only; a file that cannot be played gives an alert that is read, and the event plays what it did. Play is unavailable for None and plays also while sounds are off. Reset All puts the focus on the table, which reads Default for every event.
- [ ] Blocked: the list reads as Blocked senders, each row as the name with the shortened Contact ID. Unblock is unavailable without a selection, and after the last one the focus goes to the list, which is read as empty, and then "No sender is blocked." is announced. With the window closed (Command-W) right after the last Unblock, that is not said in another window.
- [ ] Receiving: "Limit the size of an item that someone can send me" reads as a check box. Largest size and Unit read their names, and are unavailable without the limit; "When an item is over the limit" reads its choice. A size that cannot be used gives an alert that is read, and the field shows the last size again.
- [ ] General: the Change… button after Copy reads as "Your Contact ID: Change…". Its alert is read, with how many transfers end; Cancel (Escape) changes nothing. After Change, the next alert reads the new Contact ID, with nothing said about being offline in between; the field shows it, and Copy My Contact ID copies the new one. File > Change My Contact ID… does the same from the main window. With a transfer under way and an offer waiting, both end and the offer's window closes.
- [ ] General: "Warn when a temporary file is too large for its volume" reads as a check box. With the folder for temporary files on an MS-DOS (FAT) volume, sending a folder of more than 4 GB brings a sheet that is read, with Choose Another Folder…, Continue Anyway, Cancel (Escape) and the check box "Don't warn me again"; after another folder is chosen the transfer starts without the sheet, and Settings shows that folder. The same sheet comes on Accept for a folder of that size saved to such a volume.
- [ ] Each field and check box reads its label. Changing one takes effect at once.
- [ ] Your Contact ID can be read, and Copy says that it was copied.
- [ ] Language says that it applies the next time Dodavo opens.

## Other windows

- [ ] About reads the version, the notice and the website, which opens.
- [ ] License: the pop-up button reads its label, and the text can be read from the start.
- [ ] Update Available: the release notes can be read. Install Update reports its progress in words.
- [ ] Help opens in the browser, in the language of the program, with headings to go by.
