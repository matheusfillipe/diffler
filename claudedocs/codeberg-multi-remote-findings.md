# Codeberg mirror + multi-remote CI — research findings

Task #11. Correcting my first-pass optimism: reusing the same GitHub Actions on
Codeberg is **not turnkey**. The durable value is the diffler multi-remote view.

## 1. Mirror GitHub → Codeberg — easy

- Codeberg **disables automatic pull-mirrors**, so mirror by **pushing from GitHub**.
- Cleanest automatic route: a `mirror.yml` GitHub workflow on push + tags that pushes
  to Codeberg, using a Codeberg token secret (e.g. `yesolutions/mirror-repository-action`
  or `roostorg/mirror`). Alternative: `git remote set-url --add --push origin <codeberg>`
  (only mirrors what you personally push).
- Low effort, no diffler code.

## 2. Run the SAME CI on Codeberg — partial / real work

Three independent blockers, in increasing severity:

- **Workflow directory**: docs document only `.forgejo/workflows`; Forgejo's source also
  scans `.github/workflows` but it's undocumented/version-dependent. Treat as "likely
  discovered, verify empirically; worst case copy or symlink into `.forgejo/workflows`."
- **`uses:` resolution**: a bare `uses: actions/checkout@v6` resolves against
  `DEFAULT_ACTIONS_URL = https://data.forgejo.org` (Forgejo's mirror), **not GitHub**.
  diffler's CI leans on third-party Rust actions that are unlikely to be mirrored there:
  `dtolnay/rust-toolchain`, `Swatinem/rust-cache`, `taiki-e/install-action`,
  `EmbarkStudios/cargo-deny-action`, `astral-sh/setup-uv`, `cachix/install-nix-action`,
  `crate-ci/typos`, `rust-lang/crates-io-auth-action`. Each would need rewriting to an
  absolute `uses: https://github.com/<owner>/<repo>@<ref>` (Forgejo supports absolute URLs).
  → a divergent workflow file, not a clean reuse.
- **Runner availability (biggest)**: Codeberg's **hosted Actions is limited/gated** —
  their own docs recommend Woodpecker CI or **bringing your own runner**. So actually
  executing diffler's CI on Codeberg likely needs a **self-hosted Forgejo runner**, or
  accepting Woodpecker (a separate `.woodpecker.yml`, different syntax).

Net: "reuse the same github actions on Codeberg" ≈ a derived `.forgejo/workflows/ci.yml`
with absolute `uses:` + a self-hosted runner, kept roughly in sync with `.github`. The
release pipeline (6 build targets, OIDC publish) is impractical to mirror; only the
test/lint side is worth running on Codeberg.

## 3. diffler multi-remote view — feasible, the durable value

Today: `detect.rs` inspects only `origin` and classifies GitHub vs GitLab; `mod.rs`
`build_provider` returns **one** provider. The vision ("look at all remotes, list as
much as possible") needs:

- **Enumerate all git remotes**, parse each host, detect its forge
  (`github.com`→GitHub, `codeberg.org`/Forgejo host→Forgejo, `*gitlab*`→GitLab).
- **A `ForgejoProvider`** over HTTP (`reqwest`, already in-tree) against the Forgejo REST
  API (`/api/v1/repos/{o}/{r}/actions/...`, `Authorization: token <pat>`). This is the
  [[ci-http-transport-oauth-idea]] direction. API parity with the `CiProvider` trait:
  - `list_runs` → `/actions/tasks` (or `/actions/runs`, recently added, PR #7699). OK.
  - `run_detail` → derive the `needs` DAG from the workflow YAML (same as the GitHub
    provider already does); the API omits edges. OK.
  - `job_log` → logs API is still maturing (active dev). Likely partial → degrade via
    `Capabilities`.
  - `run_extras` → artifacts API exists; annotations likely absent → empty default.
  - `current_pr` → Forgejo PR API exists. OK.
- **Aggregate**: the runs/Status view assumes a single provider; it must hold N providers
  and merge/label runs by remote.

The `CiProvider` trait already fits a Forgejo backend; `Capabilities` lets it advertise
reduced log support honestly. So the provider is a medium task; the UI aggregation is the
other half.

## Recommended phasing

0. **De-risk (ops, low code)**: create the Codeberg repo, push-mirror from GitHub, enable
   Actions, and try a *minimal* `.forgejo/workflows/ci.yml` with a self-hosted runner —
   empirically learn what actually runs before writing diffler code.
1. **diffler multi-remote**: enumerate all remotes + per-remote provider selection; add the
   HTTP `ForgejoProvider` with honest capabilities.
2. **Aggregate** multiple providers in the runs/Status view, grouped by remote.

## Open risks

- Codeberg hosted-runner availability (may force self-hosted) — top feasibility risk.
- Forgejo Actions API maturity (logs/jobs endpoints in flux) — provider may ship degraded.
- Two workflow files to keep in sync if reuse isn't clean.

## Sources
- forgejo.org/docs/latest/user/actions/{overview,basic-concepts,reference}
- docs.codeberg.org/ci/actions/
- codeberg.org/Recommendations/Mirror_to_Codeberg
- codeberg.org/forgejo/forgejo/pulls/7699 (runs API)
