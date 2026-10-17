---
name: firebrik-sites
description: Publish or update an HTML page or a whole static site (HTML, JS, CSS, images) on Firebrik Sites (sites.firebrik.com) from any project, with proof it landed intact. Use when the user asks to publish, deploy, host, share or update a page, report, prototype, app or site "on Sites", "on firebrik sites", "on my sites", or gives a sites.firebrik.com URL to change. Also use when a Sites publish fails or the user asks why publishing doesn't work.
---

# Firebrik Sites

Publish through the harness command. It works from any directory, needs no
project checkout, and only reports success after checking the result.

```sh
@HARNESS@ sites doctor                     # what publishing needs, what is missing
@HARNESS@ sites publish PATH ROUTE [NAME]  # create or update /{username}/ROUTE
@HARNESS@ sites list                       # id, route and URL of every page
```

PATH is one of:

- an `.html` file: becomes the page's `index.html`; other files of an existing
  page are kept;
- a folder with `index.html` at its root: zipped and published as the whole
  site, replacing every file of an existing page (hidden files such as `.git`
  are left out);
- a `.zip` with `index.html` at its root (or inside one top folder): published
  as the whole site.

Files are stored as uploaded, exactly like a dashboard upload: `<script>`
tags, `<script src="app.js">`, forms and event handlers all keep working.
Allowed file types are html, htm, css, js, json, md, mmd, txt, xml, yml,
yaml, png, jpg, jpeg, gif, svg, ico, woff, woff2, ttf and eot; the upload
limit is 50 MB (100 MB unpacked).

## Publishing

1. Write the page or site to disk first (a scratchpad or the project). Use
   relative paths between its files (`src="app.js"`, `href="css/site.css"`).
   Never paste a large page into a tool call; the command reads from disk, so
   an output limit cannot cut it off.
2. Pick ROUTE: lowercase letters, digits and dashes (`q3-report`). Reusing a
   route updates that page; a new route creates one. Run `sites list` when
   unsure whether a route exists, and confirm with the user before replacing
   a page you did not create in this session.
3. Run `@HARNESS@ sites publish PATH ROUTE "Readable name"`.
4. Report to the user only what the command printed:
   - `OK: <url>` means the server received exactly the uploaded bytes, every
     local file is stored with the same sha256, and the live URL serves the
     page and its assets. Give the user the URL.
   - Pass on every `WARN:` line (for example a page that looks cut off).
   - Anything else is a failure. Never claim the page is published without
     the `OK:` line.

Exit codes: 0 published and confirmed; 1 publish or verification failed
(the reason is printed); 2 bad arguments; 3 setup missing.

## When it cannot publish

On exit 3, or when the user asks why publishing fails, run
`@HARNESS@ sites doctor` and relay every `MISSING` line with its `fix:` line.
Do the fixes that are yours to do; hand the rest to the user:

- **No API token / token rejected**: only the user can create one, at
  `https://sites.firebrik.com/settings?tab=api-tokens`. Ask them to save it
  in their own terminal with the command doctor prints (it reads the token
  without echoing it). Never ask for the token in chat, never print it,
  never write it into config.json, a repository or a command line.
- **Token file readable by others**: `chmod 600` the file.
- **Token may not publish (pages:write)**: the user needs a token with full
  access from the dashboard.
- **Plan limit**: the free plan allows 3 sites; updating an existing route
  still works.
- **Server failed (5xx, "Scheduled Maintenance")**: the Sites app is down or
  broken; say so. Fixing it is work in the Firebrik Sites repository
  (its logs, secrets and deploy), not a retry loop.
- **No upload endpoint**: Sites is older than 0.1.3 and needs a deploy.
- **Publishing turned off**: the user disabled it in the harness config;
  ask before turning it back on.

## Settings

`@HARNESS@ config show` prints the `sites` section of the harness config:
`url` (default `https://sites.firebrik.com`), `token_file` (default
`~/.config/firebrik-sites/token`) and `enabled`. For one shell,
`FIREBRIK_SITES_URL`, `FIREBRIK_SITES_TOKEN_FILE` and `FIREBRIK_SITES_TOKEN`
override them; for example `FIREBRIK_SITES_URL=http://localhost:3030` targets a
local Sites checkout.
