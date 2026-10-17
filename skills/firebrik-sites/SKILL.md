---
name: firebrik-sites
description: Publish or update an HTML page on Firebrik Sites (sites.firebrik.com) from any project, with proof it landed intact. Use when the user asks to publish, deploy, host, share or update a page, report, prototype or site "on Sites", "on firebrik sites", "on my sites", or gives a sites.firebrik.com URL to change. Also use when a Sites publish fails or the user asks why publishing doesn't work.
---

# Firebrik Sites

Publish through the harness command. It works from any directory, needs no
project checkout, and only reports success after checking the result.

```sh
@HARNESS@ sites doctor                     # what publishing needs, what is missing
@HARNESS@ sites publish FILE ROUTE [NAME]  # create or update /{username}/ROUTE
@HARNESS@ sites list                       # id, route and URL of every page
```

## Publishing

1. Write the page to a file first (a scratchpad or the project), as one
   self-contained HTML document: inline CSS, absolute URLs for anything
   external. Never paste a large page into a tool call; the command reads the
   file from disk so an output limit cannot cut it off.
2. Pick ROUTE: lowercase letters, digits and dashes (`q3-report`). Reusing a
   route updates that page; a new route creates one. Run `sites list` when
   unsure whether a route exists.
3. Run `@HARNESS@ sites publish FILE ROUTE "Readable name"`.
4. Report to the user only what the command printed:
   - `OK: <url>` means the server got exactly the local bytes (sha256 match),
     stored them unchanged, and the live URL serves them. Give the user the URL.
   - Every `WARN:` line must be passed on. "the sanitizer changed the page"
     means scripts, forms, iframes or `on…` attributes were removed (and the
     sanitizer can also drop visible text shaped like `word = value`).
   - Anything else is a failure. Never claim the page is published without
     the `OK:` line.

Exit codes: 0 published and confirmed; 1 publish or verification failed
(the reason is printed); 2 bad arguments; 3 setup missing.

## When it cannot publish

On exit 3, or when the user asks why publishing fails, run
`@HARNESS@ sites doctor` and relay every `MISSING` line with its `fix:` line.
Do the fixes that are yours to do; hand the rest to the user:

- **No API token / wrong token**: only the user can create one, at
  `https://sites.firebrik.com/settings?tab=api-tokens`. Ask them to save it
  in their own terminal with the command doctor prints (it reads the token
  without echoing it). Never ask for the token in chat, never print it,
  never write it into config.json, a repository or a command line.
- **Token file readable by others**: `chmod 600` the file.
- **Server failed (5xx, "Scheduled Maintenance")**: the Sites app is down or
  broken; say so. Fixing it is work in the Firebrik Sites repository
  (its logs, secrets and deploy), not a retry loop.
- **Server does not verify uploads**: Sites is older than 0.1.2 and needs a
  deploy before publishing is trustworthy.
- **Publishing turned off**: the user disabled it in the harness config;
  ask before turning it back on.

## Pages that need JavaScript

This path sanitizes HTML: `<script>`, forms, iframes and event handlers are
removed. If the page depends on them, tell the user before publishing; the
dashboard editor and zip upload at sites.firebrik.com publish scripts
intact.

## Settings

`@HARNESS@ config show` prints the `sites` section of the harness config:
`url` (default `https://sites.firebrik.com`), `token_file` (default
`~/.config/firebrik-sites/token`) and `enabled`. For one shell,
`FIREBRIK_SITES_URL`, `FIREBRIK_SITES_TOKEN_FILE` and `FIREBRIK_SITES_TOKEN`
override them; for example `FIREBRIK_SITES_URL=http://localhost:3030` targets a
local Sites checkout.
