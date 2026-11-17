# Domain glossary

- **Collection:** A project directory whose `.dynws` storage defines its sessions.
- **Session:** Named metadata describing a focused set of repository links.
- **Workspace:** The directory containing a session's repository symlinks.
- **Managed worktree:** A Git worktree stored under the collection's managed
  worktree directory; ownership and identity must be checked before cleanup.
- **Creation record:** Manifest provenance used to validate strict `--reuse`;
  interactive sessions currently have no creation record.
- **Publication plan:** Resolved input to the shared publication
  operation: session metadata, links and required worktree operations. See the
  [publication decision](docs/adr/0001-session-publication.md).
- **Publication:** Making a prepared workspace and its metadata available as a
  session. It has an explicit commit point and owns failure cleanup.
