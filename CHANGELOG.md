# Changelog

All notable changes to Video Plucker (Desktop) are documented in this file.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [4.5.1] - 2026-08-23

### Fixed

- Browser-cookie downloads no longer fail with "Could not copy Chrome cookie
  database" while Chrome is running: the bundled ChromeCookieUnlock yt-dlp
  plugin is now actually loaded (it shipped since v4.4.0 but was never passed
  to yt-dlp via `--plugin-dirs`).
- Cookie profiles saved as "x.com" now serve twitter.com links (and vice
  versa), matching how the sites share logins.
- The cookie-copy failure now shows plain-language guidance instead of the
  raw error.

### Note

Version numbering continues from the last feature release line at the
maintainer's request; v4.10–v4.13 releases contained no functional changes
over v4.5.0.

## [4.13.0] - 2026-08-22

### Added

- Queue for later: failed downloads can be parked on a dedicated Queue tab
  and restarted whenever you like — nothing auto-starts, and parked items
  survive app restarts.
- Report button on failed downloads opens a pre-filled GitHub issue, with
  automatic duplicate detection pointing you at an existing report when one
  already covers the same failure.

### Changed

- Failed downloads now show a plain-language explanation instead of raw
  yt-dlp output (e.g. login-required sites point at the Cookie Manager).
  Technical details still travel with every issue report.
- Self-healing downloader: when a site breaks because the bundled yt-dlp got
  outdated, Video Plucker updates it automatically in the background and
  retries once.
- AllAnime is greyed out in Search while broken upstream; LuciferDonghua
  remains available.

### Fixed

- In-app updater now verifies update signatures correctly (the embedded
  public key was a placeholder, which made silent updates fail).

## [4.5.0] - 2026-08-13

### Added

- Download queueing: downloads now run one at a time in first-in, first-out
  order instead of all at once.
- Pause and resume: pause an in-progress download and pick it back up later.
  Partial progress is kept, and already-finished items in a playlist are skipped
  on resume.
- Links to the XyrusCode software catalogue and to the Discord community, in the
  app footer.
