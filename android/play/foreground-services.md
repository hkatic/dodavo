# Foreground service declarations

Play Console asks for each foreground service type in the manifest: what it
does, why it cannot wait or be a job, and a video of it. The service is
`DodavoService` (`android/app/.../service/DodavoService.kt`); its types are
`dataSync` and `connectedDevice`.

## dataSync

**Description:** Dodavo sends and receives files directly between the user's
devices and those of people they know. While a transfer that the user started
or accepted moves data, the service runs with a notification that shows the
transfer and its progress, so that the transfer goes on when the user leaves
the app. It stops when no data moves. On Android 15 and later, its daily limit
applies (`onTimeout` stops it; the transfer waits and continues later).

**Why not a job:** a transfer is a live connection to another device, which has
to be served while it lasts; a job could be deferred or stopped while the
other side is sending. User-initiated data transfer jobs (Android 14 and
later) are the alternative, for transfers that the user starts (see TODO.md).

**Video:** send a file to another device, leave the app, show the notification
with the progress, and the transfer finishing.

## connectedDevice

**Description:** with "Receive in the background", which the user turns on in
Settings (off by default), Dodavo stays reachable for the user's own computers
and their contacts' devices, which connect to it over the network to send
files. A notification, "Ready to receive", shows this for as long as it runs,
and it starts again after the device restarts only while the setting is on.

**Why not a job:** the other device connects at a moment that it decides; a job
cannot keep the app reachable for that.

**Video:** turn on Receive in the background in Settings, show the
notification, close the app, send a file from a computer, and show the offer
arriving as a notification.

If Google does not accept `connectedDevice` for this, the fallback is
`specialUse` with the same description (subtype "peer-to-peer file transfer
listener"), or shipping without the setting.
