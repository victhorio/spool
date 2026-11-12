---
name: publishing-artifacts
description: Use when the user asks for an artifact or to revise one, or when a result is a document they will come back to or share, such as a report, analysis or design to review.
---

# Publishing artifacts

Publish without being asked only for a finished document the user will come back to or share, such as a report, an analysis, or a substantial design or proposal to review. Status, progress, validation results and short plans belong in chat, the PR or the commit, which already keep them. Afterwards, give the user the link with a one-line summary rather than repeating the content in chat.

Put one deliverable in one artifact. Markdown suits most documents; use HTML only when the content needs interaction or layout Markdown can't give. An image can't be inlined, so publish one as its own artifact only when it's worth seeing, and embed it with its `content_url`.

To revise a document, update the same artifact so its link stays the same, preferring `spool_edit_artifact` for small changes. Revise it when its content changes, not to record the work's progress.
