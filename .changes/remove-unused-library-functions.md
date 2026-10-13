---
cargo-rail = "minor"
---

Remove unused public library items.
No command used them, and the command-line interface is unchanged:

- 38 functions with no production caller, including the `SystemGit` remote helpers
  (`add_remote`, `push_to_remote`, `fetch_from_remote`, `ls_remote_has_content`),
  unused `WorkspaceGraph` dependent traversals, and unused `toml` editor methods;
- the `workspace::WorkspaceView` and `CrateInfo` facade;
- `UnifyOutputFormat`, replaced by `TextJsonOutputFormat` with the same values;
- `toml::WorkspaceDepsBuilder` and `toml::editor::TomlBatchEditor`;
- `WorkspaceContext::build_with_source_capture`.

Surface backups no longer embed the configuration in `metadata.json`.
Nothing read it, and a backup whose embedded configuration held a key
that a later release retired could not be listed or restored.
