# Project tabs

`workspace::Workspace` holds the open projects as tabs
between the main loop and the apps. Each tab is a whole `App` over its own
repository, with its own screens, review state, watcher and workers, so
switching back finds a tab exactly where it was left.
- Routing: we give every tab its own channel and tag its events with the
  tab's id in a forwarder (`workspace::forward`), so a worker's result
  goes to the tab that asked even while another is in front. Keys, mouse
  and focus go to the tab in front; ticks, resizes and the agent's waiting
  signal go to every tab. A background tab sees `Focus(false)`.
- Keys: `[keys.tabs]` (`Context::Tabs`; alt with a digit or `h`/`l`/`n`/`w`
  by default) resolves before the app sees a key, read from the config of
  the project in front. The app turns a tab action into
  `App::pending_tab` (`app/tabs.rs`), which the workspace carries out, and
  refuses to close a tab while a draft or a dialog is open. Help and the
  palette list the tab actions on every screen.
- Screen: the tab row draws through `screen_chrome` only while more than
  one project is open. `alt-n` opens a picker over the git repositories
  beside the open ones, or the folders a typed path completes to, with
  `tab` filling in the selected one.
- Agent: one MCP server serves every tab. Comments, feedback and reviews
  merge across tabs and name their `project`. A call with an id goes to
  the tab owning it (`App::owns_id`). `get_diff`, `add_comment`,
  `mark_viewed` and `publish_walkthrough` take `project` (a folder name,
  or a path when two names clash) and default to the tab in front.
  `open_project` opens a tab behind the one in front. The workspace keeps
  its own feedback epoch and bumps it whenever any tab's moves, so closing
  a tab never moves it back.
- Worktrees: a git worktree or jj workspace is a folder, so it opens as a
  tab like any project. `Vcs::worktrees` lists them from git's own record
  (`.git/worktrees/`), so we see the ones an agent or a shell made wherever
  their folders live. The status screen's Worktrees group, the `w` menu,
  the picker and the worktree screen (`app/worktree.rs`) open, create and
  remove them. We run the add and remove through `git worktree` or
  `jj workspace` (`Vcs::worktree_argv`), so hooks and checkout filters run
  as in a shell; a remove git refuses for local changes asks again with
  `--force`, and a removed worktree closes its tab (`TabOp::Drop`). `w` on
  a pull request fetches its head into a branch of the same name and adds
  a worktree for it. `[worktree] path` places new ones.
- Lifetime: we do not save open tabs between runs. We write the endpoint
  file and a registry entry into every open project, so a proxy started
  in any of them finds this diffler, and the proxy counts one url as one
  instance.
