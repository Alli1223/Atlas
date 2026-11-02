# Integrations

All credentials are added under **Settings → Integrations** by an admin. They are encrypted
with `ATLAS_MASTER_KEY`, never shown again after saving, and re-checked every 24 hours: if a
provider starts rejecting one (or it nears its expiry date), every admin gets a notification.
A manual *Validate* is limited to one probe per credential per 30 seconds.

## GitHub

1. Create a fine-grained personal access token with *Contents: read & write*, *Pull requests:
   read & write* and, for webhooks, *Webhooks: read & write* on the repositories Atlas should use.
2. Add it as a **GitHub** credential.
3. On a project's board, *Link a repo* and pick the repository.

Atlas then creates `feature/KEY-123-summary` branches and pull requests from cards, shows
commits, CI and reviews on the card, moves a card to Done when its PR merges, and applies
smart-commit directives (`KEY-123 #done #comment Fixed #time 2h`). With `ATLAS_PUBLIC_URL` set a
webhook is installed; otherwise Atlas polls every five minutes.

Every linked repository is checked hourly. A revoked token, a deleted repository, or a deleted
credential shows as a warning on the card's development panel; a renamed repository is followed
automatically (GitHub keeps its numeric id).

## Claude Code

Atlas runs the local `claude` CLI against a card: *Run with Claude* on the card. By default it
uses the CLI's own login (a Max/Pro subscription). Adding an **Anthropic** API key and choosing
it in the project's agent settings (`PUT /api/v1/projects/{key}/agent-settings`; no settings
screen yet) bills the API instead — an explicit choice, never a silent fallback. Per project
you choose the permission mode (`plan`, `default`, `acceptEdits`, or
`bypassPermissions` with a warning), allowed/disallowed tools, and what happens on completion
(comment a summary, move the card, open a PR). `ATLAS_AGENT_MAX_CONCURRENT` runs at once; the
rest queue. The agent can move its own card through the Atlas MCP server
(`atlas_move_card`, `atlas_set_status`, `atlas_comment_card`, `atlas_create_card`, …).

## Gemini

Add a **Gemini** API key (sent in the `x-goog-api-key` header, never in a URL). Projects can then
generate cover art and avatars, cards can generate reference images, and the text model can
draft card descriptions or turn plain English into AQL — today through the API
(`POST /api/v1/projects/{key}/generate/images`, `POST /api/v1/generate/text`); the
pick-from-a-grid screen is not built yet. Image generation has no free tier; each
generation's cost is recorded and shown on the cost report.

## Email

Configure SMTP with `PUT /api/v1/admin/smtp` and check it with `POST /api/v1/admin/smtp/test`
(the password is stored in the vault). Each user then picks, under **Settings**, per notification
kind, *in app*, *in app + email* or *off*, and may opt into a daily digest.
