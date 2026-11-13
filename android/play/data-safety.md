# Data safety

Proposed answers for Play Console's Data safety form, from what the app does
(see PRIVACY.md). The developer runs no server and receives nothing. Google's
definitions decide; where they leave a choice, it is marked **Decide**.

## Data collection and security

- **Does your app collect or share any of the required user data types?**
  Yes. Dodavo itself sends nothing to the developer, but the relay and name
  servers of n0, which iroh uses, receive the device's IP address and Contact
  ID, and Google counts what third-party services receive.
- **Is all of the user data collected by your app encrypted in transit?** Yes
  (QUIC with TLS 1.3 to the relays, HTTPS and DNS to the name server).
- **Do you provide a way for users to request that their data is deleted?**
  **Decide.** There is no account and nothing is kept by the developer;
  uninstalling deletes the Contact ID, and the name server forgets a Contact
  ID that is no longer published. The honest answer may be "No" with that
  explanation in the privacy policy.

## Data types

| Data type | Collected | Shared | Why | Optional |
| --- | --- | --- | --- | --- |
| Device or other IDs (the Contact ID, a public key) | Yes | No | App functionality | No |
| Approximate location: **Decide** whether the IP address counts (Google counts IP addresses under it only where used for location; Dodavo does not) | Not if it does not count | No | App functionality | No |
| Files and docs | No: end-to-end encrypted between the two devices, which Google does not count as collected | No | | |
| Name (the name the user enters, sent to recipients) | No: goes only to the person the user sends to, which Google counts as neither collected nor shared | No | | |
| Crash logs and diagnostics (`Dodavo.log`, see PRIVACY.md) | No: kept on the device, and sent only where the user shares it (About Dodavo > Share log), to whom the user chooses, which Google counts as neither collected nor shared | No | | |

Not processed ephemerally: the name server keeps a published Contact ID with
the relay it can be reached through until it is published no more.

## Other forms

- **Ads:** none.
- **Target audience:** 18 and over (the app is not directed at children).
- **Content rating:** a utility; user-to-user file transfer, which the
  questionnaire asks about as sharing content between users.
- **App access:** everything is available without an account.
- **News app, health, financial features, government app:** no.
