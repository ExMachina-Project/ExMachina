# Unified command directory

The command directory is the cross-entry contract for CLI, TUI, chat, WebUI,
HTTP, and extension commands. The Rust types in
`crates/exm-core/src/command.rs` are the protocol source of truth; the entry
matrix is the checked-in human-readable projection in
[`command-entry-matrix.md`](./command-entry-matrix.md).

## Protocol

- `protocolVersion` is currently `1`; `commandVersion` is the descriptor
  version (currently `1` for built-ins).
- A `CommandDescriptor` declares the canonical name, aliases, JSON argument
  schema, scope, permissions, side effect, supported entries, output formats,
  and stable error categories.
- A directory query returns descriptors even when they are unavailable. Such
  entries have `executable: false` and an `unavailableReason`; they must not be
  presented as executable actions.
- `CommandResult` uses stable `status`/`error.category` values. Error text is
  presentation-only and is redacted before serialization.
- Directory availability is a capability snapshot, not proof that a handler
  completed successfully. Execution still performs target binding, permission,
  dependency, plan-mode, and audit checks.

## HTTP capability queries

Authenticated HTTP clients can query the current directory without causing a
command side effect:

```text
GET /api/commands?sessionId=<id>&scope=session
GET /api/capabilities?sessionId=<id>&scope=session
```

Optional query fields are `sessionId`, `scope` (`global`, `project`, `session`,
or `flow`), `protocolVersion=1`, and `includeDeprecated=true`; unsupported
protocol versions are rejected before returning entries. A pinned project session must be
queried through its `X-Exm-Workspace` project connection. The response keeps
the HTTP entry, authenticated actor, project/session binding, protocol version,
available command names, capability statuses, and registry diagnostics.

`/api/openapi.json` documents these endpoints alongside the REST surface.
These endpoints only query metadata. They do **not** add a generic HTTP command
executor: entries without an HTTP command adapter are returned with
`executable: false` and `unsupported_entry`. Existing REST handlers remain the
execution surface until an explicit adapter is implemented and tested.

## CLI and TUI projections

```text
exm help
exm help spec.export
exm --output-format json command catalog
exm command catalog --markdown
```

`exm help` and TUI `/help` are generated from the same current directory;
the TUI additionally keeps legacy slash aliases for compatibility. The JSON
catalog form is intended for scripts; the Markdown form produces the table
used by the entry matrix. TUI `/help` prints the legacy compatibility help and
then the current directory snapshot, including unavailable reasons and dynamic
skill/plugin entries. `exm command list` remains the dynamic extension-only
view, while `command catalog` is the complete built-in-plus-extension view.

## Aliases, migration, and deprecation

Aliases resolve to exactly one canonical descriptor and never create a second
handler. Existing short forms remain supported where they are listed in the
descriptor (`model`, `queue`, `goal`, `spec`, and so on); new integrations
should use the dotted canonical name.

An old command can remain registered with `deprecated: true` and an optional
`replacement`. Deprecated entries are omitted from normal help/directory
queries, can be inspected with `includeDeprecated=true`, and are rejected at
dispatch with `unsupported` plus the replacement. This keeps migration visible
without silently executing an old command or allowing a plugin to replace a
core owner.

## Entry matrix maintenance

`command-entry-matrix.md` is generated with `exm command catalog --markdown`.
The CLI test loads that artifact and asserts every built-in canonical name is
present, so adding a built-in command without updating the matrix fails
verification. Dynamic plugin/skill rows are intentionally runtime data and are
reported by the live directory rather than hard-coded in this document.
