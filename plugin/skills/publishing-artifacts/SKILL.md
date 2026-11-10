---
name: publishing-artifacts
description: Use when the user asks for an artifact, or when a result is meant to be read or kept rather than skimmed in chat, such as a plan, report, findings or chart, or to revise one published earlier.
---

# Publishing artifacts

Publish without being asked when a result is meant to be read or kept, such as a plan, report or findings. Afterwards, give the user the link with a one-line summary rather than repeating the content in chat.

Markdown suits most documents; use HTML for interactive or visual pages. Publish each image as its own artifact and embed it with its `content_url`, in Markdown or HTML.

To revise a document, update the same artifact so its link stays the same, preferring `spool_edit_artifact` for small changes.
