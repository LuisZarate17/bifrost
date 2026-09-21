# G4 — Conversations survive a restart

One JSON file per conversation in `%APPDATA%\odin\conversations`. **Built 2026-09-19.**

Beside `config.json`, in the same format. **No database:** G4 needs save, list, rename and delete,
and none of that needs a query.

## Measured

| | |
|---|---|
| Save then reload | **byte-identical** |
| The list | sorts newest first |
| `delete` of something already gone | returns `false`, does not error |
| A record written without the G5/G6 fields | still parses |
| `store` tests | **7**, of 18 in `odin-core` |

## Why a plain file beats a database here

What a file buys, for a tool one person runs:

- You can **read it in an editor** when something looks wrong.
- You can **back it up by copying it**.
- You can **delete one conversation by deleting one file**.

Indexing buys none of that, and nothing in this app queries across conversations. The trade only
flips if search arrives.

## The record carries fields nothing reads yet

`images`, `tool_calls`, `model`, `created_at` — all present at G4, all unused at G4.

That is STAGE-G decision #2: **adding a field later means migrating files that already hold real
conversations.** The cost of carrying them now is that they serialize away when empty, so a plain
text turn is still `{role, content, created_at}` on disk. The cost of adding them later is a
migration path and a version field.

G5 and G6 filled two of the four within the day, which is the argument made concretely.

**`StoredMessage` is deliberately not `chat::Message`.** One is a *file format* that has to stay
readable by future versions; the other is a *wire format* that should stay free to follow Ollama's
API. Keeping them apart means a change to one is not silently a migration of the other.

## Sized before deciding, not after

**A 1080p screenshot is ~790 KB base64-encoded.** Inlining that would turn a 20 KB conversation
into a multi-megabyte file **rewritten on every turn**. So images are stored as filenames and the
bytes live in their own files — decided at G4, implemented at G5, and the figure is why.

## Two failures that should not be possible, refused anyway

- **A file that will not parse is skipped**, not fatal to the whole list. One corrupt file should
  cost you one conversation, not all of them.
- **An id containing a path separator is refused outright.** Ids are generated here, so that should
  be impossible — which is exactly why it is worth refusing rather than trusting.

## Writes are atomic; saves are per exchange

Written to a temporary file and renamed, so **a crash mid-write leaves the previous version rather
than half a file.** Saving happens once per *completed* exchange rather than per token, which also
avoids persisting half an answer.

## No date dependency

`chrono` for one function is a poor trade, so the RFC 3339 stamp is **Howard Hinnant's
`civil_from_days` arithmetic written out** — six lines. RFC 3339 was chosen mostly because it
**sorts as a string**, which is what the list ordering relies on.

## Titles derive from the first question

A list where everything is called "New conversation" is a list you cannot use, and asking for a
title *before* the first message is a question nobody wants to answer.

## The bug this chunk shipped

`persist()` sent `title: ""` and `created_at: ""` and `save_conversation` wrote them verbatim — so
**renaming a conversation and then sending one more message silently erased the name** and reset
the date the list sorts by. Found by a code review the same day.

The command merges now: **the caller owns the messages, the file owns its title and creation
time.** A write path that accepts empty strings as "no opinion" cannot tell them from "set it to
empty", and the wrong guess is silent.

## What needed a human

Reopening the app and continuing a conversation. In [PENDING-CHECKS.md](../PENDING-CHECKS.md).
