# Bundled filter lists

`easylist.txt` and `easyprivacy.txt` in this folder are EasyList and
EasyPrivacy, copied unmodified from https://easylist.to/. They come with
Crossbow so that Block ads and trackers works without adding a list.

## Licence

The EasyList authors dual-license the lists under the GNU General Public
License, version 3 or later, and the Creative Commons Attribution-ShareAlike
3.0 Unported licence (https://easylist.to/pages/licence.html).

Crossbow uses them under **CC BY-SA 3.0 Unported**
(https://creativecommons.org/licenses/by-sa/3.0/), unmodified. This choice
covers these two files only. The rest of Crossbow reads them as data and
keeps its own licence.

## Attribution

Settings → Privacy → Content Blocking shows this, and so does the README:

> EasyList and EasyPrivacy by The EasyList authors (https://easylist.to/), used unmodified under CC BY-SA 3.0 (https://creativecommons.org/licenses/by-sa/3.0/).

## Versions and updates

`SOURCES` gives each file's version, the day it was fetched, its size, its
SHA-256 and its address. Nothing in Crossbow downloads a list. To update,
run `cargo xtask update-lists` by hand: it replaces both files and
`SOURCES` only when both new files pass its checks. Then review the diff,
update the pinned hashes in
`crates/crossbow-core/src/content_blocking/bundled_tests.rs`, run the
tests, and commit.
