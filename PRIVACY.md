# Dodavo privacy policy

Effective 12 November 2026. This policy covers Dodavo on every system: Android, Windows and macOS.

Dodavo is made by Hrvoje Katić. Questions about this policy go to the project's issues on GitHub: https://github.com/hkatic/dodavo/issues.

## In short

Dodavo sends files directly from one device to another, end-to-end encrypted. The developer runs no server, and never receives your files, your recipients or anything else about you. Dodavo has no accounts, no analytics, no advertising and no tracking.

## What stays on your device

- Your Contact ID and the secret key that proves it is yours.
- Your name as you entered it, your recipients (their names and Contact IDs), the senders you blocked and your settings.
- The records of transfers that are under way, so that they can continue after a restart. Each one is deleted when its transfer ends, and what is left of an ended transfer after 30 days at most.
- What you receive, in the folder where Dodavo saves received items, and temporary copies of what you send, until the transfer no longer needs them.
- On Windows and on a Mac, your chat conversations: the messages you wrote and received, with their times, until you delete a conversation or all of them.

Where this is kept: in Dodavo's own storage on Android, in `%APPDATA%\Dodavo` on Windows, and in `~/Library/Application Support/Dodavo` on a Mac. Nobody else receives any of it from Dodavo.

On Android, Android's own backup, if you have turned it on, includes your recipients, blocked senders and settings, but not your secret key, so a restored copy gets a new Contact ID. Google keeps that backup under its own terms.

## What others receive

**The person you send to** receives, besides the item itself: your Contact ID, your name as you entered it, the item's name, size and dates, and the IP addresses of your device, which a direct connection needs. **The person who sends to you** receives your Contact ID, your device's IP addresses, and whether you accepted, declined or cancelled. A sender you blocked learns only that the offer was declined.

**The person you chat with** (on Windows and on a Mac) receives your messages, your Contact ID, your name as you entered it, the IP addresses of your device and, unless you turn that off in Settings, that you are writing a message. Messages go directly to that person's device, end-to-end encrypted like files, and are kept there as they are on yours. Someone whose messages you do not accept learns only that.

**Devices on your local network** can see your Contact ID and the addresses of your device on that network, which Dodavo announces there (mDNS) so that devices nearby find each other.

**The servers of n0** (n0.computer), the makers of iroh, the networking library that Dodavo uses:

- Relay servers help two devices find a direct path, and pass data on when there is none. They see your device's IP address and Contact ID. What passes through them is end-to-end encrypted, so they cannot read it.
- A name server keeps, for each Contact ID, which relay server it can be reached through, so that others can find it. It can see the IP address of a device that publishes its Contact ID there or looks one up.

## Updates

On Windows and macOS, Dodavo looks for a new version on GitHub once a day, unless you turn that off in Settings, and downloads it from GitHub when you choose to install it. GitHub sees your IP address, as it does for any download. On Android, Google Play installs the updates, and Dodavo does not look for them itself.

## Google Play

If you installed Dodavo from Google Play, Google may send the developer reports of crashes and of how well the app runs, if your device's settings allow it. Google collects them under its own privacy policy.

## Permissions on Android

- **Notifications**: to tell you about items that arrive and transfers that end while you are in another app.
- **Nearby devices** (local network, Android 17 and later): to find and connect to devices on your local network.
- **Files and media** (Android 8 to 10 only): to save received items in Download/Dodavo.
- **Network access, running in the foreground, starting at boot**: to send and receive, to keep a transfer running while Dodavo is in the background, and, with Receive in the background turned on, to be ready to receive after the device restarts.

## Deleting your data

Uninstalling Dodavo on Android deletes everything in its own storage. On Windows and macOS, delete the folder named above after uninstalling. Received items stay where they were saved until you delete them. Nothing about you is kept anywhere else by the developer.

## Children

Dodavo is not directed at children.

## Changes

Changes to this policy are published in this file, with a new date at the top. Its history is in the project's repository.
