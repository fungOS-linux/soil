# Conversations

Normalized threads and messages for mail, chat, IRC, Matrix/Beeper bridges and
agent chat.

```text
fungos.conversation/thread@1
fungos.conversation/message@1
```

## Thread

```text
id
channel          email | whatsapp | matrix | irc | agent | comment
external-id
title
participants[]
visibility
first-at-ms
last-at-ms
```

## Message

```text
id
thread-id
channel
external-id
direction
sender
sender-principal?
body
attachments[]
mentions[]
source-refs[]
occurred-at-ms
edited-at-ms?
deleted
```

## Attachments are content references

Messages never contain media bytes.

```json
{
  "name": "brief.pdf",
  "media_type": "application/pdf",
  "size": 182344,
  "content_id": "blake3:...",
  "provider_refs": [
    { "provider": "ipfs", "locator": "bafy..." },
    { "provider": "s3", "locator": "s3://bucket/brief.pdf" }
  ],
  "source_ref": "mxc://matrix.example/..."
}
```

- `ContentId` is identity.
- `Locator` is a provider address.
- `source_ref` is provenance, not authority.
- VFS/IPFS/S3/local own the bytes.
- Geode owns credentials for providers.
- `soil:media`/`soil:vfs`/`soil:storage` resolve the bytes.

Thumbnails and extracted text are separate derived attachments/content ids.

## Read path

Do not scan the whole message collection to render a thread. A production host
keeps a derived index:

```text
thread_id, occurred_at_ms, message_id
```

Pagination uses a cursor:

```text
before = (occurred_at_ms, message_id)
```

`ConversationIndex` in `crates/soil-conversation` documents the cursor semantics
and is used for tests/small hosts. The persistent host index should match it.

## Write path

```text
Matrix/bridge sync batch
  -> normalize visible messages
  -> fungos.conversation/message records
  -> index update
  -> mention graph links
  -> notify only principal targets
```

Batch writes; do not fsync per message. Raw Matrix events, typing, presence,
receipts, reactions and E2EE device state stay in Matrix/bridge, not records.

## Media

```text
Matrix mxc:// event
  -> bridge/host downloads and decrypts
  -> content store writes bytes
  -> message gets content_id + provider refs
  -> source_ref keeps mxc:// provenance
```

A content reference never grants access. The book grant and host policy decide
whether a Spore can resolve the content.

## Search and RAG

Message text may be indexed by FTS/embeddings, but vectors and chunks are
derived. A hit is a message locator; the authoritative message still comes from
records.

## Provider ownership

```text
Matrix / bridges   provider protocol and raw event source
Soil records       normalized durable conversation state
Conversation index derived thread/time/unread queries
VFS/IPFS/S3/local  bytes
Geode              provider and Matrix credentials
ohmOS              UI/Spores
```
