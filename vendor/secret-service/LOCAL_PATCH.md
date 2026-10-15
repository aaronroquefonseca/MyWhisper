# Local no-prompt API patch

This is the authenticated `secret-service` 5.2.0 package. `UPSTREAM.json` records its original registry checksum, upstream commit and file hashes. Original third-party licenses are preserved; they do not establish a MyWhisper project license.

The only Rust source change adds `Collection::create_item_no_prompt`, sharing the existing encryption and CreateItem implementation. If authorization requires a prompt, it sends Dismiss and returns `Error::Prompt` without ever sending Prompt. Existing `create_item` behavior and public signatures are preserved. MyWhisper uses the new API inside its five-second operation deadline.

Intercepting method replies from another stream races the caller's continuation. This API makes the decision directly on the CreateItem result before a prompt can be activated. The private-D-Bus regression in `src/database_key.rs` exercises both the existing prompt behavior and the explicit dismissal behavior, including a delayed Dismiss acknowledgement.
