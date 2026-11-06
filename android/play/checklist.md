# Releasing on Google Play

What a release on Google Play takes besides the bundle. The bundle comes from
`android/release.sh`.

## Once

- [x] A Google Play developer account. A personal account created after
      2023-11-13 has to run a closed test with at least 12 testers for 14 days
      before it can publish to production.
- [ ] Trader status (EEA): declare it in Play Console; a trader's address and
      phone number are shown on the store page.
- [ ] Create the app as `com.hrvojekatic.dodavo` (it can never change), with
      Play App Signing, and register the upload key's certificate.
- [ ] Store listing (`listing.md`), with screenshots of a phone and a tablet in
      each language, and the app icon at 512 × 512 (from
      `android/app/src/main/res/drawable/ic_launcher_foreground.xml`).
- [ ] Privacy policy URL: https://github.com/hkatic/dodavo/blob/master/PRIVACY.md
      (it is on `master` only after a merge).
- [ ] Data safety (`data-safety.md`), content rating, target audience, ads
      (none), app access (no login).
- [ ] Foreground service declarations with videos (`foreground-services.md`).
- [ ] Local network permission declaration (apps that target Android 17; due
      2027-08-25).

## Each release

- [ ] `android/release.sh` on a clean checkout, with the upload key.
- [ ] Upload to internal testing; read the pre-launch report (crashes,
      accessibility, 16 KB page support in the App bundle explorer).
- [ ] Release notes in each language (`release-notes/<version>/`).
- [ ] Promote to closed testing, then to production.
