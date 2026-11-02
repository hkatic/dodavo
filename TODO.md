# To do

What still has to be done, most important first. Defects are in [ISSUES.md](ISSUES.md), together with what it takes to fix them.

- **High:** do this soon; until it is done, a serious problem could go unnoticed.
- **Medium:** do this when the occasion comes, or when there is time.
- **Low:** worth doing, but nothing depends on it.

Remove an item in the commit that settles it. This file is public, so keep secrets and private addresses or paths out of it.

## High

- **Install the published 1.0.0 from the release page.** Nobody has run the published setup program yet. Try a new installation for the current user and for all users, an installation over an older copy, and uninstalling.
- **Send between two computers on different networks.** All live tests so far ran on one PC, so connecting through real NAT, and falling back to a relay there, has never been tried.
- **Go through the main tasks with real screen readers.** Use NVDA, JAWS and Narrator. The scripted checks only cover what MSAA and UI Automation expose, not what is actually spoken.
- **Back up the update signing key.** Keep copies in at least two places besides the PC that signs the releases. If the key is lost, installed copies can never accept an automatic update again.

## Medium

- **Run the first real update when the next version is out.** Downloading and installing an update has only been tested against a local web server. Against the real GitHub URL, only the check has run so far, which answered that 1.0.0 is the latest version.
- **Test on Windows 7 SP1 and Windows 8.1.** No such system has been available. The builds for them pass the import check and start on Windows 11, and the README says that they are provided as is.
- **Run transfers with the 32-bit builds.** For 1.0.0 they were only started and closed on Windows 11.
- **Check the notifications live.** Windows 11 notifications were not visible to the test scripts, so only unit tests cover them.

## Low

- **Report the zip crate's panic to its authors.** `enclosed_name` panics on ordinary names such as "Kuća.jpg" in zip 9.1 and 9.2, because `is_windows_reserved_device_name` splits a name at a byte index. Dodavo avoids it with its own check (`archive::entry_path`). There is no record that this was reported.
